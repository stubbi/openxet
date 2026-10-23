//! The SQLite implementation of [`Index`].

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use openxet_protocol::MerkleHash;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{Row, Sqlite, Transaction};

use crate::types::{decode_chunk_table, decode_terms, encode_chunk_table, encode_terms, hash_from};
use crate::{ChunkTable, FileRecord, Index, IndexError, NewFile, Visibility};

/// With the `fault-injection` feature, `register_shard` for this scope fails
/// after writing its first file, so tests can check the transaction rolls
/// back.
#[cfg(feature = "fault-injection")]
pub const FAULT_AFTER_FIRST_FILE_SCOPE: &str = "openxet-test/fault-after-first-file";

/// How long a write waits for another connection's write lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Hashes bound per `IN (...)` query, well under SQLite's variable limit.
const HASHES_PER_QUERY: usize = 500;

/// A xorb `x` is visible when visibility is global (first bind) or it is
/// registered to the caller's scope (second bind).
const VISIBLE: &str =
    "(? OR EXISTS (SELECT 1 FROM xorb_scopes s WHERE s.xorb = x.hash AND s.scope = ?))";

/// The index in one SQLite database file, in WAL mode with `synchronous=FULL`
/// so a committed write survives a crash or power loss (design §5.9 req 7).
/// Several processes may open the same file; writers serialize on SQLite's
/// write lock.
#[derive(Debug, Clone)]
pub struct SqliteIndex {
    pool: SqlitePool,
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
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT);
        let pool = SqlitePoolOptions::new().connect_with(options).await?;
        sqlx::migrate!()
            .run(&pool)
            .await
            .map_err(IndexError::Migrate)?;
        Ok(Self { pool })
    }

    /// Starts a write transaction holding the write lock from the start, so
    /// its reads and writes see one state and it never fails to upgrade.
    async fn begin_write(&self) -> Result<Transaction<'static, Sqlite>, IndexError> {
        Ok(self.pool.begin_with("BEGIN IMMEDIATE").await?)
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

/// Inserts `(chunk, xorb)` dedup candidates, skipping ones already present.
async fn insert_dedup(
    tx: &mut Transaction<'_, Sqlite>,
    chunks: &[MerkleHash],
    xorb: &MerkleHash,
) -> Result<(), IndexError> {
    for batch in chunks.chunks(HASHES_PER_QUERY) {
        let rows = vec!["(?, ?)"; batch.len()].join(", ");
        let sql = format!("INSERT INTO dedup (chunk, xorb) VALUES {rows} ON CONFLICT DO NOTHING");
        let mut query = sqlx::query(&sql);
        for chunk in batch {
            query = query.bind(chunk.as_bytes()).bind(xorb.as_bytes());
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
        let sql = format!("SELECT x.size FROM xorbs x WHERE x.hash = ? AND {VISIBLE}");
        let size: Option<i64> = sqlx::query_scalar(&sql)
            .bind(xorb.as_bytes())
            .bind(vis == Visibility::Global)
            .bind(scope)
            .fetch_optional(&self.pool)
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
            let params = vec!["?"; batch.len()].join(", ");
            let sql = format!(
                "SELECT x.hash, x.chunk_count, x.chunk_table FROM xorbs x \
                 WHERE x.hash IN ({params}) AND {VISIBLE}"
            );
            let mut query = sqlx::query(&sql);
            for hash in batch {
                query = query.bind(hash.as_bytes());
            }
            let rows = query
                .bind(vis == Visibility::Global)
                .bind(scope)
                .fetch_all(&self.pool)
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
    ) -> Result<bool, IndexError> {
        let sizes = files
            .iter()
            .map(|f| to_i64(f.record.size, "file size"))
            .collect::<Result<Vec<_>, _>>()?;
        let mut tx = self.begin_write().await?;
        let mut new_for_scope = sqlx::query(
            "INSERT INTO shard_scopes (shard, scope) VALUES (?, ?) ON CONFLICT DO NOTHING",
        )
        .bind(shard.as_bytes())
        .bind(scope)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        for (i, (file, size)) in files.iter().zip(sizes).enumerate() {
            let record = &file.record;
            sqlx::query(
                "INSERT INTO files (hash, size, terms) VALUES (?, ?, ?) ON CONFLICT DO NOTHING",
            )
            .bind(record.hash.as_bytes())
            .bind(size)
            .bind(encode_terms(&record.terms))
            .execute(&mut *tx)
            .await?;
            new_for_scope |= sqlx::query(
                "INSERT INTO file_scopes (file, scope, sha256, shard) VALUES (?, ?, ?, ?) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(record.hash.as_bytes())
            .bind(scope)
            .bind(file.sha256.as_ref().map(<[u8; 32]>::as_slice))
            .bind(shard.as_bytes())
            .execute(&mut *tx)
            .await?
            .rows_affected()
                > 0;
            if let (Some(first), Some(term)) = (file.first_chunk, record.terms.first()) {
                insert_dedup(&mut tx, &[first], &term.xorb).await?;
            }
            fault_point(i, scope)?;
        }
        tx.commit().await?;
        Ok(new_for_scope)
    }

    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError> {
        let row = sqlx::query(
            "SELECT f.size, f.terms FROM files f JOIN file_scopes s ON s.file = f.hash \
             WHERE f.hash = ? AND s.scope = ?",
        )
        .bind(file.as_bytes())
        .bind(scope)
        .fetch_optional(&self.pool)
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
        let sql = format!(
            "SELECT x.hash FROM dedup d JOIN xorbs x ON x.hash = d.xorb \
             WHERE d.chunk = ? AND {VISIBLE} ORDER BY x.ingested_seq DESC LIMIT 1"
        );
        let xorb: Option<Vec<u8>> = sqlx::query_scalar(&sql)
            .bind(chunk.as_bytes())
            .bind(vis == Visibility::Global)
            .bind(scope)
            .fetch_optional(&self.pool)
            .await?;
        xorb.as_deref().map(hash_from).transpose()
    }
}
