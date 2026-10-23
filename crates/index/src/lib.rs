//! openxet's index: which xorbs, files and shards exist, which scopes they
//! are registered to, and which xorb to offer for a dedup query.
//!
//! The index is rebuildable: the bucket is the source of truth (design
//! §5.3), and every row can be recomputed from stored xorbs and shards.
//! Nodes are stateless, so implementations must stay correct with several
//! processes writing at once; SQLite ships now and Postgres comes behind the
//! same [`Index`] trait in M3 (design §5.2, §5.8).

#![forbid(unsafe_code)]

mod sqlite;
mod types;

use std::collections::HashMap;

use openxet_protocol::MerkleHash;

#[cfg(feature = "fault-injection")]
pub use sqlite::FAULT_AFTER_FIRST_FILE_SCOPE;
pub use sqlite::SqliteIndex;
pub use types::{ChunkTable, FileRecord, NewFile, Scope, Term, Visibility};

/// The index's operations. Every write is idempotent: content is addressed by
/// hash, so repeating a call with the same arguments changes nothing and
/// reports `false`.
///
/// `scope` is the caller's scope (design §5.4). Files are always visible only
/// in the scopes they were registered to. Xorbs are content-addressed and
/// shared; whether another scope's xorb is visible depends on [`Visibility`].
#[async_trait::async_trait]
pub trait Index: Send + Sync + 'static {
    /// Records a stored xorb of `size` bytes with its chunk table, registers
    /// it to `scope`, and records `eligible` (its chunks that pass
    /// `is_dedup_eligible`) as dedup candidates.
    ///
    /// Returns `true` if the xorb was not yet registered to `scope`. A xorb
    /// that already exists keeps its size, table and ingestion order; only
    /// the scope membership and dedup candidates are added. Reporting per
    /// scope means the answer never reveals another scope's uploads.
    ///
    /// # Errors
    ///
    /// [`IndexError::InvalidInput`] if `table` doesn't have `n + 1` offsets
    /// for `n` chunks, and [`IndexError::Database`] if the write fails.
    /// Nothing is recorded on error.
    async fn put_xorb(
        &self,
        xorb: &MerkleHash,
        size: u64,
        table: &ChunkTable,
        eligible: &[MerkleHash],
        scope: &str,
    ) -> Result<bool, IndexError>;

    /// The stored size of `xorb`, or `None` if it is missing or not visible
    /// to `scope` under `vis`.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn xorb_size(
        &self,
        xorb: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<u64>, IndexError>;

    /// The chunk tables of `xorbs` that are stored and visible to `scope`
    /// under `vis`. A missing or invisible xorb is absent from the map;
    /// duplicates in `xorbs` are answered once.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn chunk_tables(
        &self,
        xorbs: &[MerkleHash],
        scope: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkTable>, IndexError>;

    /// Registers the files of a verified shard to `scope`, in one
    /// transaction: either everything is recorded or nothing is.
    ///
    /// - Each file is registered to `scope` with its own reconstruction,
    ///   SHA-256 and `shard`. Reconstructions are kept per scope (ruling
    ///   M1-R16): the same file hash may be stored over different xorbs in
    ///   different scopes, and each scope reads back only its own.
    /// - A file already registered to `scope` keeps its first registration;
    ///   a later shard's terms for it describe the same bytes and are
    ///   ignored.
    /// - Each file's `first_chunk` becomes a dedup candidate for its first
    ///   term's xorb.
    ///
    /// Returns `true` if the shard or any of its files was newly registered
    /// to `scope`, `false` if all of it already was.
    ///
    /// # Errors
    ///
    /// [`IndexError::InvalidInput`] if a size doesn't fit the store, and
    /// [`IndexError::Database`] if the write fails, including when a file
    /// references a xorb that isn't stored. Nothing is recorded on error.
    async fn register_shard(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        scope: &str,
    ) -> Result<bool, IndexError>;

    /// The reconstruction `scope` registered for `file`, or `None` if it
    /// isn't registered to `scope`. Another scope's registration is never
    /// returned, under any [`Visibility`] (design §5.4, ruling M1-R16).
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError>;

    /// The xorb to offer for a global dedup query on `chunk`: of the visible
    /// xorbs that list `chunk` as a dedup candidate, the one first ingested
    /// most recently. `None` if there is none.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn dedup_xorb(
        &self,
        chunk: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<MerkleHash>, IndexError>;
}

/// Errors from the index. Messages name no credentials or secrets; scopes
/// and hashes are not secret.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    /// The database could not be opened, read or written.
    #[error(
        "index database error; check that the data directory is writable and the disk is not \
         full, then retry"
    )]
    Database(#[source] sqlx::Error),
    /// The schema could not be brought up to date.
    #[error(
        "could not migrate the index schema; the database may be from a newer openxet, so run \
         that version or move the index aside and rebuild it from the bucket"
    )]
    Migrate(#[source] sqlx::migrate::MigrateError),
    /// A stored row doesn't decode. The bucket is the source of truth, so
    /// the index can be rebuilt from it.
    #[error(
        "the index holds a corrupt entry ({0}); the bucket is unaffected, so rebuild the index \
         from it, and please report this as a bug"
    )]
    Corrupt(String),
    /// The caller passed data the index can't store, which is a bug in the
    /// caller.
    #[error("invalid index input: {0}; this is a bug in openxet, please report it")]
    InvalidInput(String),
    /// A failure injected by the `fault-injection` test feature.
    #[cfg(feature = "fault-injection")]
    #[error("injected fault (test builds only)")]
    InjectedFault,
}

impl From<sqlx::Error> for IndexError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(e)
    }
}
