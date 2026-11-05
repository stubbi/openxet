//! The Postgres implementation of [`Index`] (ruling M3-R2, ADR 0015).
//!
//! Several nodes write one database at once, so every write is safe
//! against a concurrent identical one: content-addressed rows are inserted
//! with `ON CONFLICT DO NOTHING`, so a lost race is success, and batches
//! are inserted in key order, so two writers of overlapping rows wait for
//! each other instead of deadlocking. What Postgres still aborts, a
//! serialization failure or a deadlock, is retried up to [`RETRIES`] times
//! and then answered with [`IndexError::Busy`].

use std::collections::{HashMap, HashSet};
#[cfg(feature = "fault-injection")]
use std::sync::Arc;
#[cfg(feature = "fault-injection")]
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use openxet_protocol::MerkleHash;
use sqlx::postgres::{PgConnection, PgPool, PgPoolOptions};
use sqlx::{Postgres, Row, Transaction};

use crate::types::{
    TERM_LEN, decode_chunk_layout, decode_chunk_table, decode_term_bytes, decode_terms,
    encode_chunk_layout, encode_chunk_table, encode_term_bytes, encode_terms, fault_point,
    hash_from, term_bytes, to_i64, to_u64, to_usize,
};
use crate::{
    ChunkLayout, ChunkTable, FileRecord, Index, IndexError, NewFile, Registrant, TermBytes,
    Visibility,
};

/// How often a write aborted by a serialization failure or a deadlock is
/// tried again before [`IndexError::Busy`] (ruling M3-R2).
pub const RETRIES: u32 = 3;

/// The pause before retry `n` (from 1) is `n` times this, so the
/// transaction that won has time to commit.
const RETRY_PAUSE: Duration = Duration::from_millis(10);

/// Whether migrations run when the index is opened (ruling M3-R6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Migrate {
    /// Apply pending migrations on connect, under Postgres' advisory lock,
    /// so nodes starting together migrate once.
    Auto,
    /// Leave the schema as it is; an operator migrates it.
    Off,
}

/// The index in a Postgres database, shared by every node.
#[derive(Debug, Clone)]
pub struct PostgresIndex {
    pool: PgPool,
    /// Commits still to fail with a serialization failure, so a test can
    /// drive the retries.
    #[cfg(feature = "fault-injection")]
    failing_commits: Arc<AtomicU32>,
}

impl PostgresIndex {
    /// Connects to the database at `url` (`postgres://...`) with at most
    /// `max_connections` connections, and with [`Migrate::Auto`] applies
    /// pending migrations.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] if the database can't be reached, and
    /// [`IndexError::Migrate`] if the schema can't be migrated.
    pub async fn connect(
        url: &str,
        max_connections: u32,
        migrate: Migrate,
    ) -> Result<Self, IndexError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(url)
            .await?;
        if migrate == Migrate::Auto {
            sqlx::migrate!("./migrations-postgres")
                .run(&pool)
                .await
                .map_err(IndexError::Migrate)?;
        }
        Ok(Self {
            pool,
            #[cfg(feature = "fault-injection")]
            failing_commits: Arc::new(AtomicU32::new(0)),
        })
    }

    /// The database this index is connected to, so a test can open a
    /// second handle on it, as a second node would.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn database(&self) -> Option<String> {
        self.pool
            .connect_options()
            .get_database()
            .map(str::to_owned)
    }

    /// Makes the next `n` write transactions of this handle fail at commit
    /// with a serialization failure (`40001`) raised by Postgres itself.
    #[cfg(feature = "fault-injection")]
    pub fn fail_next_commits(&self, n: u32) {
        self.failing_commits.store(n, Ordering::SeqCst);
    }

    /// Commits `tx`.
    async fn commit(&self, tx: Transaction<'static, Postgres>) -> Result<(), IndexError> {
        #[cfg(feature = "fault-injection")]
        let tx = self.inject_conflict(tx).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Fails `tx` with a serialization failure if
    /// [`PostgresIndex::fail_next_commits`] asked for one.
    #[cfg(feature = "fault-injection")]
    async fn inject_conflict(
        &self,
        mut tx: Transaction<'static, Postgres>,
    ) -> Result<Transaction<'static, Postgres>, IndexError> {
        if self
            .failing_commits
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            sqlx::query(
                "DO $$ BEGIN RAISE EXCEPTION 'injected serialization failure' \
                 USING ERRCODE = '40001'; END $$",
            )
            .execute(&mut *tx)
            .await?;
        }
        Ok(tx)
    }
}

/// Whether Postgres aborted the transaction for a conflict with a
/// concurrent one, which trying again resolves: a serialization failure
/// (`40001`) or a deadlock (`40P01`).
fn is_conflict(e: &IndexError) -> bool {
    match e {
        IndexError::Database(sqlx::Error::Database(db)) => {
            matches!(db.code().as_deref(), Some("40001" | "40P01"))
        }
        _ => false,
    }
}

/// Runs `attempt` until it doesn't fail with a conflict, at most
/// [`RETRIES`] times more than once.
async fn retrying<T, Fut>(mut attempt: impl FnMut() -> Fut) -> Result<T, IndexError>
where
    Fut: Future<Output = Result<T, IndexError>>,
{
    let mut retries = 0;
    loop {
        match attempt().await {
            Err(e) if is_conflict(&e) => {
                if retries == RETRIES {
                    tracing::warn!(retries, "index write kept conflicting; answering busy");
                    let IndexError::Database(last) = e else {
                        unreachable!("is_conflict holds only for database errors")
                    };
                    return Err(IndexError::Busy(last));
                }
                retries += 1;
                tokio::time::sleep(RETRY_PAUSE * retries).await;
            }
            done => return done,
        }
    }
}

/// `items` without repeats, in their first order.
fn unique<T: Copy + Eq + std::hash::Hash>(items: impl IntoIterator<Item = T>) -> Vec<T> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|x| seen.insert(*x)).collect()
}

/// The raw bytes of `hashes`, as a `BYTEA[]` binds them.
fn raw(hashes: &[MerkleHash]) -> Vec<&[u8]> {
    hashes.iter().map(MerkleHash::as_bytes).collect()
}

/// `hashes` without repeats, in byte order: the order batches insert in,
/// so concurrent writers lock shared rows in one order.
fn sorted(hashes: impl IntoIterator<Item = MerkleHash>) -> Vec<MerkleHash> {
    let mut out: Vec<MerkleHash> = unique(hashes);
    out.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    out
}

/// The layouts of `xorbs` visible to `tenant` under `vis`, read on `conn`,
/// as [`Index::chunk_layouts`] answers them.
async fn layouts_in(
    conn: &mut PgConnection,
    xorbs: &[MerkleHash],
    tenant: &str,
    vis: Visibility,
) -> Result<HashMap<MerkleHash, ChunkLayout>, IndexError> {
    let xorbs = unique(xorbs.iter().copied());
    let rows = sqlx::query(
        "SELECT x.hash, x.chunk_count, l.chunk_layout \
         FROM xorbs x JOIN xorb_layouts l ON l.xorb = x.hash \
         WHERE x.hash = ANY($1) AND ($2 OR EXISTS \
             (SELECT 1 FROM xorb_owners o WHERE o.xorb = x.hash AND o.tenant = $3))",
    )
    .bind(raw(&xorbs))
    .bind(vis == Visibility::Global)
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await?;
    let mut layouts = HashMap::with_capacity(rows.len());
    for row in rows {
        let hash = hash_from(row.try_get::<&[u8], _>(0)?)?;
        let count = to_usize(row.try_get(1)?, "chunk count")?;
        layouts.insert(
            hash,
            decode_chunk_layout(row.try_get::<&[u8], _>(2)?, count)?,
        );
    }
    Ok(layouts)
}

/// The first of `xorbs`, in order, that isn't visible to `tenant` under
/// `vis`.
async fn first_invisible(
    conn: &mut PgConnection,
    xorbs: &[MerkleHash],
    tenant: &str,
    vis: Visibility,
) -> Result<Option<MerkleHash>, IndexError> {
    let ordered = unique(xorbs.iter().copied());
    let rows: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT x.hash FROM xorbs x WHERE x.hash = ANY($1) AND ($2 OR EXISTS \
         (SELECT 1 FROM xorb_owners o WHERE o.xorb = x.hash AND o.tenant = $3))",
    )
    .bind(raw(&ordered))
    .bind(vis == Visibility::Global)
    .bind(tenant)
    .fetch_all(&mut *conn)
    .await?;
    let visible = rows
        .iter()
        .map(|r| hash_from(r))
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(ordered.into_iter().find(|x| !visible.contains(x)))
}

/// Inserts `(chunk, xorb)` dedup candidates in key order, skipping ones
/// already present.
async fn insert_dedup(
    conn: &mut PgConnection,
    mut pairs: Vec<(MerkleHash, MerkleHash)>,
) -> Result<(), IndexError> {
    pairs.sort_unstable_by(|a, b| {
        (a.0.as_bytes(), a.1.as_bytes()).cmp(&(b.0.as_bytes(), b.1.as_bytes()))
    });
    pairs.dedup();
    let (chunks, xorbs): (Vec<&[u8]>, Vec<&[u8]>) = pairs
        .iter()
        .map(|(c, x)| (c.as_bytes(), x.as_bytes()))
        .unzip();
    sqlx::query(
        "INSERT INTO dedup (chunk, xorb) SELECT * FROM unnest($1::bytea[], $2::bytea[]) \
         ON CONFLICT DO NOTHING",
    )
    .bind(chunks)
    .bind(xorbs)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A file of a shard, encoded for its `file_scopes` row.
struct FileRow {
    hash: MerkleHash,
    size: i64,
    terms: Vec<u8>,
    sha256: Option<Vec<u8>>,
    term_bytes: Vec<u8>,
}

impl PostgresIndex {
    async fn put_xorb_once(
        &self,
        xorb: &MerkleHash,
        size: i64,
        count: i64,
        (blob, layout): (&[u8], &[u8]),
        eligible: &[MerkleHash],
        tenant: &str,
    ) -> Result<bool, IndexError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO xorbs (hash, size, chunk_count, chunk_table) VALUES ($1, $2, $3, $4) \
             ON CONFLICT DO NOTHING",
        )
        .bind(xorb.as_bytes())
        .bind(size)
        .bind(count)
        .bind(blob)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO xorb_layouts (xorb, chunk_layout) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(xorb.as_bytes())
        .bind(layout)
        .execute(&mut *tx)
        .await?;
        let new_for_tenant = sqlx::query(
            "INSERT INTO xorb_owners (xorb, tenant) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        )
        .bind(xorb.as_bytes())
        .bind(tenant)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        insert_dedup(&mut tx, eligible.iter().map(|c| (*c, *xorb)).collect()).await?;
        self.commit(tx).await?;
        Ok(new_for_tenant)
    }

    async fn register_shard_once(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        by: Registrant<'_>,
        seq: i64,
        vis: Visibility,
    ) -> Result<bool, IndexError> {
        let Registrant { scope, tenant, .. } = by;
        let referenced: Vec<MerkleHash> = unique(
            files
                .iter()
                .flat_map(|f| f.record.terms.iter().map(|t| t.xorb)),
        );
        let mut tx = self.pool.begin().await?;
        if let Some(xorb) = first_invisible(&mut tx, &referenced, tenant, vis).await? {
            return Err(IndexError::UnknownXorb { xorb });
        }
        let mut new_for_scope = sqlx::query(
            "INSERT INTO shard_registrations (shard, scope, tenant, registered_seq) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(shard.as_bytes())
        .bind(scope)
        .bind(tenant)
        .bind(seq)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        // A shard's rows depend only on its content, so rows already
        // present are kept.
        sqlx::query(
            "INSERT INTO shard_xorbs (shard, pos, xorb) \
             SELECT $1, t.pos - 1, t.xorb FROM unnest($2::bytea[]) WITH ORDINALITY AS t (xorb, pos) \
             ON CONFLICT DO NOTHING",
        )
        .bind(shard.as_bytes())
        .bind(raw(&referenced))
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO xorb_owners (xorb, tenant) SELECT x, $2 FROM unnest($1::bytea[]) AS x \
             ON CONFLICT DO NOTHING",
        )
        .bind(raw(&sorted(referenced.iter().copied())))
        .bind(tenant)
        .execute(&mut *tx)
        .await?;
        // Every term xorb is visible (checked above), so every layout is
        // found.
        let layouts = layouts_in(&mut tx, &referenced, tenant, vis).await?;
        let mut rows = Vec::with_capacity(files.len());
        let mut seen = HashSet::with_capacity(files.len());
        for file in files {
            let record = &file.record;
            // The same bytes: a repeat in one shard has the same number,
            // shard and tenant, so the first is the one kept.
            if !seen.insert(record.hash) {
                continue;
            }
            rows.push(FileRow {
                hash: record.hash,
                size: to_i64(record.size, "file size")?,
                terms: encode_terms(&record.terms),
                sha256: file.sha256.map(Vec::from),
                term_bytes: encode_term_bytes(&term_bytes(&record.terms, &layouts)?),
            });
        }
        rows.sort_unstable_by(|a, b| a.hash.as_bytes().cmp(b.hash.as_bytes()));
        if !rows.is_empty() {
            new_for_scope |= Self::write_files(&mut tx, &rows, shard, scope, tenant, seq).await?;
            fault_point(0, scope)?;
        }
        let firsts: Vec<(MerkleHash, MerkleHash)> = files
            .iter()
            .filter_map(|f| Some((f.first_chunk?, f.record.terms.first()?.xorb)))
            .collect();
        insert_dedup(&mut tx, firsts).await?;
        self.commit(tx).await?;
        Ok(new_for_scope)
    }

    /// Writes `rows` (in key order) to `scope`'s files for registration
    /// `seq` of `shard` by `tenant`, and reports whether any was new.
    async fn write_files(
        tx: &mut PgConnection,
        rows: &[FileRow],
        shard: &MerkleHash,
        scope: &str,
        tenant: &str,
        seq: i64,
    ) -> Result<bool, IndexError> {
        let hashes: Vec<&[u8]> = rows.iter().map(|r| r.hash.as_bytes()).collect();
        let sizes: Vec<i64> = rows.iter().map(|r| r.size).collect();
        let terms: Vec<&[u8]> = rows.iter().map(|r| r.terms.as_slice()).collect();
        let sha256: Vec<Option<&[u8]>> = rows.iter().map(|r| r.sha256.as_deref()).collect();
        let bytes: Vec<&[u8]> = rows.iter().map(|r| r.term_bytes.as_slice()).collect();
        let inserted = sqlx::query(
            "INSERT INTO file_scopes \
             (file, scope, size, terms, sha256, shard, term_bytes, registered_seq, \
             registered_tenant) \
             SELECT u.file, $1, u.size, u.terms, u.sha256, $2, u.term_bytes, $3, $4 \
             FROM unnest($5::bytea[], $6::int8[], $7::bytea[], $8::bytea[], $9::bytea[]) \
                 AS u (file, size, terms, sha256, term_bytes) \
             ON CONFLICT DO NOTHING",
        )
        .bind(scope)
        .bind(shard.as_bytes())
        .bind(seq)
        .bind(tenant)
        .bind(&hashes)
        .bind(&sizes)
        .bind(&terms)
        .bind(&sha256)
        .bind(&bytes)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if usize::try_from(inserted).is_ok_and(|n| n == rows.len()) {
            return Ok(true);
        }
        // Registered already. The row of the lowest (number, shard, tenant)
        // is kept whichever committed first, as a rebuild from the bucket
        // keeps it (ruling M1-R37); all describe the same bytes. A row
        // inserted just now has this registration's own key, so it isn't
        // changed.
        sqlx::query(
            "UPDATE file_scopes f SET size = u.size, terms = u.terms, sha256 = u.sha256, \
             shard = $2, term_bytes = u.term_bytes, registered_seq = $3, registered_tenant = $4 \
             FROM unnest($5::bytea[], $6::int8[], $7::bytea[], $8::bytea[], $9::bytea[]) \
                 AS u (file, size, terms, sha256, term_bytes) \
             WHERE f.file = u.file AND f.scope = $1 \
             AND (f.registered_seq, f.shard, f.registered_tenant) > ($3, $2, $4)",
        )
        .bind(scope)
        .bind(shard.as_bytes())
        .bind(seq)
        .bind(tenant)
        .bind(&hashes)
        .bind(&sizes)
        .bind(&terms)
        .bind(&sha256)
        .bind(&bytes)
        .execute(&mut *tx)
        .await?;
        Ok(inserted > 0)
    }
}

#[async_trait::async_trait]
impl Index for PostgresIndex {
    async fn probe(&self) -> Result<(), IndexError> {
        // A real table, so a database without the schema fails here.
        sqlx::query("SELECT 1 FROM xorbs LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(())
    }

    async fn put_xorb(
        &self,
        xorb: &MerkleHash,
        size: u64,
        table: &ChunkTable,
        eligible: &[MerkleHash],
        tenant: &str,
    ) -> Result<bool, IndexError> {
        let blob = encode_chunk_table(table)?;
        let layout = encode_chunk_layout(table);
        let size = to_i64(size, "xorb size")?;
        let count = i64::try_from(table.hashes.len())
            .map_err(|_| IndexError::InvalidInput("chunk count is over i64::MAX".to_owned()))?;
        retrying(|| self.put_xorb_once(xorb, size, count, (&blob, &layout), eligible, tenant)).await
    }

    async fn xorb_size(
        &self,
        xorb: &MerkleHash,
        tenant: &str,
        vis: Visibility,
    ) -> Result<Option<u64>, IndexError> {
        let size: Option<i64> = sqlx::query_scalar(
            "SELECT x.size FROM xorbs x WHERE x.hash = $1 AND ($2 OR EXISTS \
             (SELECT 1 FROM xorb_owners o WHERE o.xorb = x.hash AND o.tenant = $3))",
        )
        .bind(xorb.as_bytes())
        .bind(vis == Visibility::Global)
        .bind(tenant)
        .fetch_optional(&self.pool)
        .await?;
        size.map(|s| to_u64(s, "xorb size")).transpose()
    }

    async fn chunk_tables(
        &self,
        xorbs: &[MerkleHash],
        tenant: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkTable>, IndexError> {
        let xorbs = unique(xorbs.iter().copied());
        let rows = sqlx::query(
            "SELECT x.hash, x.chunk_count, x.chunk_table FROM xorbs x \
             WHERE x.hash = ANY($1) AND ($2 OR EXISTS \
                 (SELECT 1 FROM xorb_owners o WHERE o.xorb = x.hash AND o.tenant = $3))",
        )
        .bind(raw(&xorbs))
        .bind(vis == Visibility::Global)
        .bind(tenant)
        .fetch_all(&self.pool)
        .await?;
        let mut tables = HashMap::with_capacity(rows.len());
        for row in rows {
            let hash = hash_from(row.try_get::<&[u8], _>(0)?)?;
            let count = to_usize(row.try_get(1)?, "chunk count")?;
            tables.insert(
                hash,
                decode_chunk_table(row.try_get::<&[u8], _>(2)?, count)?,
            );
        }
        Ok(tables)
    }

    async fn chunk_layouts(
        &self,
        xorbs: &[MerkleHash],
        tenant: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkLayout>, IndexError> {
        let mut conn = self.pool.acquire().await?;
        layouts_in(&mut conn, xorbs, tenant, vis).await
    }

    async fn next_registration_seq(&self) -> Result<u64, IndexError> {
        let seq: i64 = sqlx::query_scalar("SELECT nextval('registration_seq')")
            .fetch_one(&self.pool)
            .await?;
        to_u64(seq, "registration sequence number")
    }

    async fn register_shard(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        by: Registrant<'_>,
        vis: Visibility,
    ) -> Result<bool, IndexError> {
        let seq = to_i64(by.seq, "registration sequence number")?;
        retrying(|| self.register_shard_once(shard, files, by, seq, vis)).await
    }

    async fn file_with_bytes(
        &self,
        file: &MerkleHash,
        scope: &str,
    ) -> Result<Option<(FileRecord, Option<TermBytes>)>, IndexError> {
        let row = sqlx::query(
            "SELECT size, terms, term_bytes FROM file_scopes WHERE file = $1 AND scope = $2",
        )
        .bind(file.as_bytes())
        .bind(scope)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            let record = FileRecord {
                hash: *file,
                size: to_u64(row.try_get(0)?, "file size")?,
                terms: decode_terms(row.try_get::<&[u8], _>(1)?)?,
            };
            let bytes = decode_term_bytes(row.try_get::<&[u8], _>(2)?, record.terms.len())?;
            Ok((record, Some(bytes)))
        })
        .transpose()
    }

    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError> {
        let row = sqlx::query("SELECT size, terms FROM file_scopes WHERE file = $1 AND scope = $2")
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

    async fn files(
        &self,
        files: &[MerkleHash],
        scope: &str,
    ) -> Result<HashMap<MerkleHash, FileRecord>, IndexError> {
        let files = unique(files.iter().copied());
        let rows = sqlx::query(
            "SELECT file, size, terms FROM file_scopes WHERE scope = $1 AND file = ANY($2)",
        )
        .bind(scope)
        .bind(raw(&files))
        .fetch_all(&self.pool)
        .await?;
        let mut records = HashMap::with_capacity(rows.len());
        for row in rows {
            let hash = hash_from(row.try_get::<&[u8], _>(0)?)?;
            let record = FileRecord {
                hash,
                size: to_u64(row.try_get(1)?, "file size")?,
                terms: decode_terms(row.try_get::<&[u8], _>(2)?)?,
            };
            records.insert(hash, record);
        }
        Ok(records)
    }

    async fn term_count(&self, files: &[MerkleHash], scope: &str) -> Result<u64, IndexError> {
        // `octet_length` of a stored value reads its length from the TOAST
        // header without fetching it, so this loads no term list.
        let bytes: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(octet_length(terms)), 0)::int8 FROM file_scopes \
             WHERE scope = $1 AND file = ANY($2)",
        )
        .bind(scope)
        .bind(raw(&unique(files.iter().copied())))
        .fetch_one(&self.pool)
        .await?;
        let bytes = to_u64(bytes, "term list length")?;
        if !bytes.is_multiple_of(TERM_LEN as u64) {
            return Err(IndexError::Corrupt(format!(
                "term lists total {bytes} bytes, not a multiple of {TERM_LEN}"
            )));
        }
        Ok(bytes / TERM_LEN as u64)
    }

    async fn dedup_xorb(
        &self,
        chunk: &MerkleHash,
        tenant: &str,
        vis: Visibility,
    ) -> Result<Option<MerkleHash>, IndexError> {
        let xorb: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT x.hash FROM dedup d JOIN xorbs x ON x.hash = d.xorb \
             WHERE d.chunk = $1 AND ($2 OR EXISTS \
                 (SELECT 1 FROM xorb_owners o WHERE o.xorb = x.hash AND o.tenant = $3)) \
             ORDER BY x.ingested_seq DESC LIMIT 1",
        )
        .bind(chunk.as_bytes())
        .bind(vis == Visibility::Global)
        .bind(tenant)
        .fetch_optional(&self.pool)
        .await?;
        xorb.as_deref().map(hash_from).transpose()
    }

    async fn dedup_shard_xorbs(
        &self,
        xorb: &MerkleHash,
        tenant: &str,
        vis: Visibility,
    ) -> Result<Option<Vec<MerkleHash>>, IndexError> {
        // One statement, so the shard and its xorbs come from one snapshot.
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT x.hash FROM shard_xorbs sx JOIN xorbs x ON x.hash = sx.xorb \
             WHERE sx.shard = (\
                 SELECT r.shard FROM shard_xorbs m \
                 JOIN shard_registrations r ON r.shard = m.shard \
                 WHERE m.xorb = $1 AND ($2 OR r.tenant = $3) \
                 ORDER BY r.registered_seq DESC LIMIT 1\
             ) AND ($2 OR EXISTS \
                 (SELECT 1 FROM xorb_owners o WHERE o.xorb = x.hash AND o.tenant = $3)) \
             ORDER BY sx.pos",
        )
        .bind(xorb.as_bytes())
        .bind(vis == Visibility::Global)
        .bind(tenant)
        .fetch_all(&self.pool)
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
