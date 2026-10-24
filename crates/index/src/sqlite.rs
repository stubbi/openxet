//! The SQLite implementation of [`Index`].

use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::Duration;

use openxet_protocol::MerkleHash;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{AssertSqlSafe, Row, Sqlite, Transaction};

use crate::types::{decode_chunk_table, decode_terms, encode_chunk_table, encode_terms, hash_from};
use crate::{ChunkTable, FileRecord, Index, IndexError, NewFile, Visibility};

/// With the `fault-injection` feature, `register_shard` for this scope fails
/// after writing its first file, so tests can check the transaction rolls
/// back.
#[cfg(feature = "fault-injection")]
pub const FAULT_AFTER_FIRST_FILE_SCOPE: &str = "openxet-test/fault-after-first-file";

/// How long a write waits for another process's write lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Hashes bound per `IN (...)` query, well under SQLite's variable limit.
const HASHES_PER_QUERY: usize = 500;

/// The SQL condition under which xorb `x` is visible: visibility is global
/// (first bind) or `x` is registered to the caller's scope (second bind). A
/// macro, so queries using it stay `&'static str`.
macro_rules! visible {
    () => {
        "(? OR EXISTS (SELECT 1 FROM xorb_scopes s WHERE s.xorb = x.hash AND s.scope = ?))"
    };
}

/// A query whose only dynamic part is `count` copies of `placeholder`, for
/// binding a batch of values. Every text part is `'static`, so no caller data
/// can enter the SQL.
fn with_placeholders(
    head: &'static str,
    placeholder: &'static str,
    count: usize,
    tail: &'static str,
) -> AssertSqlSafe<String> {
    AssertSqlSafe(format!(
        "{head}{}{tail}",
        vec![placeholder; count].join(", ")
    ))
}

/// The index in one SQLite database file, in WAL mode with `synchronous=FULL`
/// so a committed write survives a crash or power loss (design §5.9 req 7).
/// Several processes may open the same file; writers serialize on SQLite's
/// write lock.
///
/// Writes go through a pool of one connection and reads through their own
/// pool. SQLite has one write lock per file and its busy handler polls for it
/// with growing sleeps rather than queueing, so several connections of one
/// process waiting for it can starve one of them past the busy timeout
/// (<https://sqlite.org/c3ref/busy_timeout.html>). With one writer connection
/// sqlx's fair pool queues this process's writes in order, and the busy
/// handler only arbitrates between processes. WAL readers never block on the
/// writer (<https://sqlite.org/wal.html#concurrency>).
#[derive(Debug, Clone)]
pub struct SqliteIndex {
    writer: SqlitePool,
    reader: SqlitePool,
}

impl SqliteIndex {
    /// Opens the index at `path`, creating the file if it is missing, and
    /// applies pending schema migrations.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] if the file can't be opened or created, and
    /// [`IndexError::Migrate`] if the schema can't be migrated.
    pub async fn open(path: &Path) -> Result<Self, IndexError> {
        // Held until the schema is migrated: SQLite answers two first-time
        // opens (the switch to WAL, the migrations' read-then-write
        // transactions) with an immediate `SQLITE_BUSY` that its busy
        // handler never retries, so servers starting together on one data
        // directory would otherwise fail.
        let _lock = OpenLock::acquire(path).await?;
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        sqlx::migrate!()
            .run(&writer)
            .await
            .map_err(IndexError::Migrate)?;
        let reader = SqlitePoolOptions::new().connect_with(options).await?;
        Ok(Self { writer, reader })
    }

    /// Starts a write transaction holding the write lock from the start, so
    /// its reads and writes see one state and it never fails to upgrade.
    async fn begin_write(&self) -> Result<Transaction<'static, Sqlite>, IndexError> {
        Ok(self.writer.begin_with("BEGIN IMMEDIATE").await?)
    }
}

/// An exclusive advisory lock on `{index}-lock`, between processes opening
/// the same index. Released on drop.
struct OpenLock {
    _held: Option<File>,
}

/// How long an open waits for another process's open to finish.
const OPEN_LOCK_TIMEOUT: Duration = Duration::from_mins(1);

impl OpenLock {
    async fn acquire(index: &Path) -> Result<Self, IndexError> {
        let mut name = index.as_os_str().to_owned();
        name.push("-lock");
        let path = PathBuf::from(name);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let failed = |source| IndexError::OpenLock {
            path: path.clone(),
            source,
        };
        let file = match options.open(&path) {
            Ok(file) => file,
            // No directory to hold the index either: opening the database
            // reports that, with SQLite's own cause.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { _held: None });
            }
            Err(err) => return Err(failed(err)),
        };
        let deadline = tokio::time::Instant::now() + OPEN_LOCK_TIMEOUT;
        let mut waited = false;
        loop {
            match file.try_lock() {
                Ok(()) => {
                    if waited {
                        tracing::info!(path = %path.display(), "got the index lock");
                    }
                    return Ok(Self { _held: Some(file) });
                }
                Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                    if !waited {
                        // The server isn't ready (`/readyz` is 503) until
                        // the other process has opened the index.
                        tracing::info!(
                            path = %path.display(),
                            "waiting for another process to finish opening the index"
                        );
                        waited = true;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(TryLockError::WouldBlock) => {
                    return Err(failed(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "another process held it for over a minute",
                    )));
                }
                Err(TryLockError::Error(err)) => return Err(failed(err)),
            }
        }
    }
}

fn to_i64(value: u64, what: &str) -> Result<i64, IndexError> {
    i64::try_from(value)
        .map_err(|_| IndexError::InvalidInput(format!("{what} {value} is over i64::MAX")))
}

fn to_u64(value: i64, what: &str) -> Result<u64, IndexError> {
    u64::try_from(value).map_err(|_| IndexError::Corrupt(format!("{what} is negative ({value})")))
}

fn to_usize(value: i64, what: &str) -> Result<usize, IndexError> {
    usize::try_from(value)
        .map_err(|_| IndexError::Corrupt(format!("{what} {value} is not a valid count")))
}

/// Fails `register_shard` after its first file for
/// [`FAULT_AFTER_FIRST_FILE_SCOPE`].
#[cfg(feature = "fault-injection")]
fn fault_point(file_index: usize, scope: &str) -> Result<(), IndexError> {
    if file_index == 0 && scope == FAULT_AFTER_FIRST_FILE_SCOPE {
        return Err(IndexError::InjectedFault);
    }
    Ok(())
}

#[cfg(not(feature = "fault-injection"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the fault-injection build"
)]
fn fault_point(_file_index: usize, _scope: &str) -> Result<(), IndexError> {
    Ok(())
}

/// The first of `xorbs`, in order, that isn't visible to `scope` under `vis`,
/// read inside `tx` so the answer holds until it commits.
async fn first_invisible(
    tx: &mut Transaction<'_, Sqlite>,
    xorbs: &[MerkleHash],
    scope: &str,
    vis: Visibility,
) -> Result<Option<MerkleHash>, IndexError> {
    let mut seen = HashSet::with_capacity(xorbs.len());
    let ordered: Vec<MerkleHash> = xorbs.iter().copied().filter(|x| seen.insert(*x)).collect();
    let mut visible = HashSet::with_capacity(ordered.len());
    for batch in ordered.chunks(HASHES_PER_QUERY) {
        let mut query = sqlx::query_scalar::<_, Vec<u8>>(with_placeholders(
            "SELECT x.hash FROM xorbs x WHERE x.hash IN (",
            "?",
            batch.len(),
            concat!(") AND ", visible!()),
        ));
        for hash in batch {
            query = query.bind(hash.as_bytes());
        }
        let rows = query
            .bind(vis == Visibility::Global)
            .bind(scope)
            .fetch_all(&mut **tx)
            .await?;
        for row in rows {
            visible.insert(hash_from(&row)?);
        }
    }
    Ok(ordered.into_iter().find(|x| !visible.contains(x)))
}

/// Maps a foreign-key violation from writing a row that names `xorb` to
/// [`IndexError::UnknownXorb`]: the xorb isn't stored, which retrying won't
/// fix, so the generic database error would mislead.
fn unknown_xorb_on_fk_violation(e: IndexError, xorb: MerkleHash) -> IndexError {
    match e {
        IndexError::Database(sqlx::Error::Database(db)) if db.is_foreign_key_violation() => {
            IndexError::UnknownXorb { xorb }
        }
        other => other,
    }
}

/// Inserts `(chunk, xorb)` dedup candidates, skipping ones already present.
async fn insert_dedup(
    tx: &mut Transaction<'_, Sqlite>,
    chunks: &[MerkleHash],
    xorb: &MerkleHash,
) -> Result<(), IndexError> {
    for batch in chunks.chunks(HASHES_PER_QUERY) {
        let mut query = sqlx::query(with_placeholders(
            "INSERT INTO dedup (chunk, xorb) VALUES ",
            "(?, ?)",
            batch.len(),
            " ON CONFLICT DO NOTHING",
        ));
        for chunk in batch {
            query = query.bind(chunk.as_bytes()).bind(xorb.as_bytes());
        }
        query.execute(&mut **tx).await?;
    }
    Ok(())
}

/// Records the distinct xorbs of `referenced` as `shard`'s, in order of
/// first appearance. A shard's rows depend only on its content, so rows
/// already present are kept.
async fn insert_shard_xorbs(
    tx: &mut Transaction<'_, Sqlite>,
    shard: &MerkleHash,
    referenced: &[MerkleHash],
) -> Result<(), IndexError> {
    let mut seen = HashSet::with_capacity(referenced.len());
    let ordered: Vec<MerkleHash> = referenced
        .iter()
        .copied()
        .filter(|x| seen.insert(*x))
        .collect();
    let mut pos: i64 = 0;
    for batch in ordered.chunks(HASHES_PER_QUERY) {
        let mut query = sqlx::query(with_placeholders(
            "INSERT INTO shard_xorbs (shard, pos, xorb) VALUES ",
            "(?, ?, ?)",
            batch.len(),
            " ON CONFLICT DO NOTHING",
        ));
        for xorb in batch {
            query = query.bind(shard.as_bytes()).bind(pos).bind(xorb.as_bytes());
            pos += 1;
        }
        query.execute(&mut **tx).await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl Index for SqliteIndex {
    async fn put_xorb(
        &self,
        xorb: &MerkleHash,
        size: u64,
        table: &ChunkTable,
        eligible: &[MerkleHash],
        scope: &str,
    ) -> Result<bool, IndexError> {
        let blob = encode_chunk_table(table)?;
        let size = to_i64(size, "xorb size")?;
        let count = i64::try_from(table.hashes.len())
            .map_err(|_| IndexError::InvalidInput("chunk count is over i64::MAX".to_owned()))?;
        let mut tx = self.begin_write().await?;
        // `WHERE true` resolves SQLite's parse ambiguity between a join
        // constraint and the upsert clause (https://sqlite.org/lang_upsert.html).
        sqlx::query(
            "INSERT INTO xorbs (hash, size, chunk_count, chunk_table, ingested_seq) \
             SELECT ?, ?, ?, ?, COALESCE(MAX(ingested_seq), 0) + 1 FROM xorbs WHERE true \
             ON CONFLICT (hash) DO NOTHING",
        )
        .bind(xorb.as_bytes())
        .bind(size)
        .bind(count)
        .bind(blob)
        .execute(&mut *tx)
        .await?;
        let new_for_scope = sqlx::query(
            "INSERT INTO xorb_scopes (xorb, scope) VALUES (?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(xorb.as_bytes())
        .bind(scope)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        insert_dedup(&mut tx, eligible, xorb).await?;
        tx.commit().await?;
        Ok(new_for_scope)
    }

    async fn xorb_size(
        &self,
        xorb: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<u64>, IndexError> {
        let size: Option<i64> = sqlx::query_scalar(concat!(
            "SELECT x.size FROM xorbs x WHERE x.hash = ? AND ",
            visible!()
        ))
        .bind(xorb.as_bytes())
        .bind(vis == Visibility::Global)
        .bind(scope)
        .fetch_optional(&self.reader)
        .await?;
        size.map(|s| to_u64(s, "xorb size")).transpose()
    }

    async fn chunk_tables(
        &self,
        xorbs: &[MerkleHash],
        scope: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkTable>, IndexError> {
        let unique: Vec<&MerkleHash> = {
            let mut seen = HashSet::with_capacity(xorbs.len());
            xorbs.iter().filter(|h| seen.insert(**h)).collect()
        };
        let mut tables = HashMap::with_capacity(unique.len());
        for batch in unique.chunks(HASHES_PER_QUERY) {
            let mut query = sqlx::query(with_placeholders(
                "SELECT x.hash, x.chunk_count, x.chunk_table FROM xorbs x WHERE x.hash IN (",
                "?",
                batch.len(),
                concat!(") AND ", visible!()),
            ));
            for hash in batch {
                query = query.bind(hash.as_bytes());
            }
            let rows = query
                .bind(vis == Visibility::Global)
                .bind(scope)
                .fetch_all(&self.reader)
                .await?;
            for row in rows {
                let hash = hash_from(row.try_get::<&[u8], _>(0)?)?;
                let count = to_usize(row.try_get(1)?, "chunk count")?;
                let table = decode_chunk_table(row.try_get::<&[u8], _>(2)?, count)?;
                tables.insert(hash, table);
            }
        }
        Ok(tables)
    }

    async fn register_shard(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        scope: &str,
        vis: Visibility,
    ) -> Result<bool, IndexError> {
        let sizes = files
            .iter()
            .map(|f| to_i64(f.record.size, "file size"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut tx = self.begin_write().await?;
        let referenced: Vec<MerkleHash> = files
            .iter()
            .flat_map(|f| f.record.terms.iter().map(|t| t.xorb))
            .collect();
        if let Some(xorb) = first_invisible(&mut tx, &referenced, scope, vis).await? {
            return Err(IndexError::UnknownXorb { xorb });
        }
        // `WHERE true` as in `put_xorb`.
        let mut new_for_scope = sqlx::query(
            "INSERT INTO shard_scopes (shard, scope, registered_seq) \
             SELECT ?, ?, COALESCE(MAX(registered_seq), 0) + 1 FROM shard_scopes WHERE true \
             ON CONFLICT DO NOTHING",
        )
        .bind(shard.as_bytes())
        .bind(scope)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        insert_shard_xorbs(&mut tx, shard, &referenced).await?;
        for (i, (file, size)) in files.iter().zip(sizes).enumerate() {
            let record = &file.record;
            new_for_scope |= sqlx::query(
                "INSERT INTO file_scopes (file, scope, size, terms, sha256, shard) \
                 VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(record.hash.as_bytes())
            .bind(scope)
            .bind(size)
            .bind(encode_terms(&record.terms))
            .bind(file.sha256.as_ref().map(<[u8; 32]>::as_slice))
            .bind(shard.as_bytes())
            .execute(&mut *tx)
            .await?
            .rows_affected()
                > 0;
            if let (Some(first), Some(term)) = (file.first_chunk, record.terms.first()) {
                insert_dedup(&mut tx, &[first], &term.xorb)
                    .await
                    .map_err(|e| unknown_xorb_on_fk_violation(e, term.xorb))?;
            }
            fault_point(i, scope)?;
        }
        tx.commit().await?;
        Ok(new_for_scope)
    }

    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError> {
        let row = sqlx::query("SELECT size, terms FROM file_scopes WHERE file = ? AND scope = ?")
            .bind(file.as_bytes())
            .bind(scope)
            .fetch_optional(&self.reader)
            .await?;
        row.map(|row| {
            Ok(FileRecord {
                hash: *file,
                size: to_u64(row.try_get(0)?, "file size")?,
                terms: decode_terms(row.try_get::<&[u8], _>(1)?)?,
            })
        })
        .transpose()
    }

    async fn dedup_xorb(
        &self,
        chunk: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<MerkleHash>, IndexError> {
        let xorb: Option<Vec<u8>> = sqlx::query_scalar(concat!(
            "SELECT x.hash FROM dedup d JOIN xorbs x ON x.hash = d.xorb WHERE d.chunk = ? AND ",
            visible!(),
            " ORDER BY x.ingested_seq DESC LIMIT 1"
        ))
        .bind(chunk.as_bytes())
        .bind(vis == Visibility::Global)
        .bind(scope)
        .fetch_optional(&self.reader)
        .await?;
        xorb.as_deref().map(hash_from).transpose()
    }

    async fn dedup_shard_xorbs(
        &self,
        xorb: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<Vec<MerkleHash>>, IndexError> {
        let global = vis == Visibility::Global;
        // One statement, so the shard and its xorbs come from one snapshot.
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(concat!(
            "SELECT x.hash FROM shard_xorbs sx JOIN xorbs x ON x.hash = sx.xorb \
             WHERE sx.shard = (\
                 SELECT ss.shard FROM shard_xorbs m \
                 JOIN shard_scopes ss ON ss.shard = m.shard \
                 WHERE m.xorb = ? AND (? OR ss.scope = ?) \
                 ORDER BY ss.registered_seq DESC LIMIT 1\
             ) AND ",
            visible!(),
            " ORDER BY sx.pos"
        ))
        .bind(xorb.as_bytes())
        .bind(global)
        .bind(scope)
        .bind(global)
        .bind(scope)
        .fetch_all(&self.reader)
        .await?;
        if rows.is_empty() {
            return Ok(None);
        }
        rows.iter()
            .map(|row| hash_from(row))
            .collect::<Result<_, _>>()
            .map(Some)
    }
}
