//! openxet's index: which xorbs, files and shards exist, which scopes they
//! are registered to, which tenants own which xorbs, and which xorb to offer
//! for a dedup query ([`Index`]); which repositories exist ([`Repos`]); and
//! their refs, packs and repack leases ([`GitStore`]).
//!
//! The index is rebuildable: the bucket is the source of truth (design
//! §5.3), and every row can be recomputed from stored xorbs and shards and
//! the bucket's registration and xorb-owner records (ruling M1-R37).
//! Nodes are stateless, so implementations must stay correct with several
//! processes writing at once; SQLite ships now and Postgres comes behind the
//! same traits in M3 (design §5.2, §5.8).

#![forbid(unsafe_code)]

mod git;
#[cfg(feature = "test-support")]
mod rebuild;
mod repos;
mod sqlite;
mod types;

use std::collections::HashMap;

use openxet_protocol::MerkleHash;

pub use git::{
    CommitError, CommitTx, GitStore, Lease, Pack, PackRecord, PackState, RefLogRow, RefUpdate,
    Reindexed, RepackPublish, Snapshot, Unplaced, UnplacedKind,
};
#[cfg(feature = "test-support")]
pub use rebuild::{FileRow, RebuildError, ScopeTables, rebuild_scope_tables};
pub use repos::{AuditEntry, Page, Repo, RepoFilter, Repos};
#[cfg(feature = "fault-injection")]
pub use sqlite::FAULT_AFTER_FIRST_FILE_SCOPE;
pub use sqlite::SqliteIndex;
pub use types::{
    ChunkLayout, ChunkTable, FileRecord, NewFile, Oid, PackId, ParseIdError, Registrant, RepoId,
    Scope, Term, TermBytes, Visibility,
};

/// The index's operations. Every write is idempotent: content is addressed by
/// hash, so repeating a call with the same arguments changes nothing and
/// reports `false`.
///
/// Files are read and registered per **scope**, the caller's unit of access:
/// they are always visible only in the scopes they were registered to.
/// Xorbs are content-addressed and shared; whether one is visible to the
/// caller's **tenant**, its unit of isolation, depends on [`Visibility`]
/// (design §5.4, ruling M1-R38).
#[async_trait::async_trait]
pub trait Index: Send + Sync + 'static {
    /// Records a stored xorb of `size` bytes with its chunk table, records
    /// `tenant` as an owner, and records `eligible` (its chunks that pass
    /// `is_dedup_eligible`) as dedup candidates.
    ///
    /// Returns `true` if `tenant` didn't own the xorb yet. A xorb that
    /// already exists keeps its size, table and ingestion order; only the
    /// owner and dedup candidates are added. Reporting per tenant means the
    /// answer never reveals another tenant's uploads.
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
        tenant: &str,
    ) -> Result<bool, IndexError>;

    /// The stored size of `xorb`, or `None` if it is missing or not visible
    /// to `tenant` under `vis`.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn xorb_size(
        &self,
        xorb: &MerkleHash,
        tenant: &str,
        vis: Visibility,
    ) -> Result<Option<u64>, IndexError>;

    /// The chunk tables of `xorbs` that are stored and visible to `tenant`
    /// under `vis`. A missing or invisible xorb is absent from the map;
    /// duplicates in `xorbs` are answered once.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn chunk_tables(
        &self,
        xorbs: &[MerkleHash],
        tenant: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkTable>, IndexError>;

    /// The [`ChunkLayout`]s of `xorbs`, as [`Index::chunk_tables`] answers
    /// their tables: what reconstruction planning reads, without loading the
    /// chunk hashes.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn chunk_layouts(
        &self,
        xorbs: &[MerkleHash],
        tenant: &str,
        vis: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkLayout>, IndexError>;

    /// A new registration sequence number, higher than every one this index
    /// handed out before. A registration takes one before it writes its
    /// bucket record (ruling M1-R37).
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] if the write fails, or
    /// [`IndexError::Corrupt`].
    async fn next_registration_seq(&self) -> Result<u64, IndexError>;

    /// Registers the files of a verified shard to `by.scope`, for
    /// `by.tenant`, as registration number `by.seq`, in one transaction:
    /// either everything is recorded or nothing is. The caller has written
    /// the registration's bucket record first (ruling M1-R37).
    ///
    /// - Every xorb named by a file's terms must be visible to `by.tenant`
    ///   under `vis` (ruling M1-R17): stored, and under
    ///   [`Visibility::PerTenant`] also owned by the tenant. Otherwise the
    ///   call fails with [`IndexError::UnknownXorb`], and the answer is the
    ///   same whether the xorb is stored elsewhere or not at all, so it
    ///   reveals nothing about other tenants.
    /// - `by.tenant` becomes an owner of every xorb the files reference, in
    ///   every visibility mode (ruling M1-R39), so a later switch to
    ///   [`Visibility::PerTenant`] never hides a registered file's xorbs
    ///   from its own tenant.
    /// - Each file is registered to the scope with its own reconstruction,
    ///   SHA-256 and `shard`. Reconstructions are kept per scope (ruling
    ///   M1-R16): the same file hash may be stored over different xorbs in
    ///   different scopes, and each scope reads back only its own.
    /// - Of several registrations of one file to one scope, the one with
    ///   the lowest `(seq, shard bytes, tenant)` is kept, whichever
    ///   committed first, as a rebuild from the bucket's records keeps it;
    ///   all describe the same bytes.
    /// - Each file's `first_chunk` becomes a dedup candidate for its first
    ///   term's xorb.
    /// - The shard's distinct term xorbs are recorded in term order (the
    ///   order of first reference across its files), for
    ///   [`Index::dedup_shard_xorbs`], where the newest registration is the
    ///   one with the highest `seq`.
    ///
    /// Returns `true` if the shard was newly registered to the scope for
    /// the tenant, or any of its files to the scope, and `false` if all of
    /// it already was.
    ///
    /// # Errors
    ///
    /// [`IndexError::UnknownXorb`] for a term xorb that isn't visible,
    /// [`IndexError::InvalidInput`] if a size or `seq` doesn't fit the
    /// store, and [`IndexError::Database`] if the write fails. Nothing is
    /// recorded on error.
    async fn register_shard(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        by: Registrant<'_>,
        vis: Visibility,
    ) -> Result<bool, IndexError>;

    /// The reconstruction `scope` registered for `file`, or `None` if it
    /// isn't registered to `scope`. Another scope's registration is never
    /// returned, under any [`Visibility`] (design §5.4, ruling M1-R16).
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError>;

    /// [`Index::file`], with the [`TermBytes`] recorded when `scope`
    /// registered it: `None` for a file registered before they were kept.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn file_with_bytes(
        &self,
        file: &MerkleHash,
        scope: &str,
    ) -> Result<Option<(FileRecord, Option<TermBytes>)>, IndexError>;

    /// The reconstructions `scope` registered for `files`, as
    /// [`Index::file`] answers each, in a few queries rather than one per
    /// file. A file not registered to `scope` is absent from the map;
    /// duplicates in `files` are answered once.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn files(
        &self,
        files: &[MerkleHash],
        scope: &str,
    ) -> Result<HashMap<MerkleHash, FileRecord>, IndexError>;

    /// The total terms of the reconstructions `scope` registered for
    /// `files`, counting each file once, as [`Index::files`] would return
    /// them, without loading or decoding any: a batch reconstruction checks
    /// its term cap with it before it loads the files (ruling M1-R40).
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn term_count(&self, files: &[MerkleHash], scope: &str) -> Result<u64, IndexError>;

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
        tenant: &str,
        vis: Visibility,
    ) -> Result<Option<MerkleHash>, IndexError>;

    /// The xorbs a global dedup answer offers alongside `xorb` (ruling
    /// M1-R30): those of the newest registered shard whose files reference
    /// `xorb`, in that shard's term order, as [`Index::register_shard`]
    /// recorded them. Under [`Visibility::PerTenant`] the shard must be
    /// registered for `tenant` and "newest" counts only `tenant`'s
    /// registrations; under [`Visibility::Global`] any tenant's count. Only
    /// xorbs visible to `tenant` under `vis` are listed, so the answer names
    /// no other xorb. `None` if no such shard lists a visible xorb.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn dedup_shard_xorbs(
        &self,
        xorb: &MerkleHash,
        tenant: &str,
        vis: Visibility,
    ) -> Result<Option<Vec<MerkleHash>>, IndexError>;
}

/// Everything the Hub layer reads and writes, as one trait object
/// (`Arc<dyn HubIndex>`): a trait object names one non-auto trait.
pub trait HubIndex: Index + Repos + GitStore {}

impl<T: Index + Repos + GitStore> HubIndex for T {}

/// Errors from the index. Messages name no credentials or secrets; scopes
/// and hashes are not secret.
///
/// Non-exhaustive, so a variant that exists only in some builds (the
/// `fault-injection` one) never changes which matches compile.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IndexError {
    /// The database could not be opened, read or written.
    #[error(
        "index database error; check that the data directory is writable and the disk is not \
         full, then retry"
    )]
    Database(#[source] sqlx::Error),
    /// The lock that serializes opening the index between processes could
    /// not be taken.
    #[error(
        "could not lock {} while opening the index; check that the data directory is writable \
         and that no other process is stuck opening it",
        .path.display()
    )]
    OpenLock {
        /// The lock file.
        path: std::path::PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
    /// The schema could not be brought up to date.
    #[error(
        "could not migrate the index schema; the database may be from a newer openxet, so run \
         that version. The bucket is unaffected; rebuilding the index from it is tracked in \
         https://github.com/stubbi/openxet/issues/26"
    )]
    Migrate(#[source] sqlx::migrate::MigrateError),
    /// A stored row doesn't decode. The bucket is the source of truth, so
    /// the index can be rebuilt from it once `reindex` exists
    /// (<https://github.com/stubbi/openxet/issues/26>).
    #[error(
        "the index holds a corrupt entry ({0}); the bucket is unaffected. Please report this at \
         https://github.com/stubbi/openxet/issues; rebuilding the index from the bucket is \
         tracked in https://github.com/stubbi/openxet/issues/26"
    )]
    Corrupt(String),
    /// A shard's file names a xorb that isn't visible to the caller's tenant:
    /// not stored, or under [`Visibility::PerTenant`] owned only by other
    /// tenants. Both cases read the same, so the error can't probe them.
    #[error(
        "a file in the shard references xorb {xorb}, which is not stored on this server; upload \
         the xorb before the shard that references it"
    )]
    UnknownXorb {
        /// The first xorb found that isn't visible.
        xorb: MerkleHash,
    },
    /// A repository of the same `{namespace}/{name}`, ignoring ASCII case,
    /// exists (ruling M2-R2).
    #[error(
        "a repository named {name} already exists (names are unique ignoring case); choose \
         another name"
    )]
    NameTaken {
        /// The `{namespace}/{name}` asked for, as written.
        name: String,
    },
    /// A repository's labels changed since the caller read them (ruling
    /// M2-R12): [`Repos::set_labels`] changed nothing.
    #[error("the labels of repository {repo} changed concurrently; read them again and retry")]
    LabelsChanged {
        /// The repository.
        repo: RepoId,
    },
    /// No repository has this id: it was never created or has been deleted.
    #[error("repository {repo} does not exist; it may have been deleted")]
    UnknownRepo {
        /// The id asked for.
        repo: RepoId,
    },
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
