//! Refs, packs, the ref log, repack leases and unplaced objects: the
//! database half of Git hosting (ADR 0004 items 3 to 5). Packs live in the
//! bucket; these rows say which of them a request may see.

use std::time::Duration;

use crate::{IndexError, Oid, PackId, RepoId};

/// A pack row's state (ADR 0004 items 3 to 5). Only `Committed` packs are
/// ever visible to a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackState {
    /// Inserted before its files are uploaded; not yet visible.
    Pending,
    /// Published by a commit transaction or a repack: visible.
    Committed,
    /// Its writer's transaction failed; GC deletes it after the grace period.
    Lost,
    /// Pending past the grace period, marked by GC so no transaction can
    /// commit it.
    Expired,
    /// Replaced by a repack's outputs; GC deletes it after the grace period.
    Superseded,
    /// Found by `reindex` with nothing a rebuilt ref reaches; never visible,
    /// never collected (ruling R24).
    Recovered,
}

impl PackState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Committed => "committed",
            Self::Lost => "lost",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
            Self::Recovered => "recovered",
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self, IndexError> {
        Ok(match s {
            "pending" => Self::Pending,
            "committed" => Self::Committed,
            "lost" => Self::Lost,
            "expired" => Self::Expired,
            "superseded" => Self::Superseded,
            "recovered" => Self::Recovered,
            other => return Err(IndexError::Corrupt(format!("pack state {other:?}"))),
        })
    }
}

/// A pack row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    /// The row's id.
    pub id: PackId,
    /// The repository it belongs to.
    pub repo: RepoId,
    /// Its file name in the bucket, without extension.
    pub name: String,
    /// Its state.
    pub state: PackState,
    /// When it was created, Unix seconds; nodes pin the pack's mtime to it
    /// (ADR 0004 item 4).
    pub created_at: u64,
    /// The size of its `.pack` file.
    pub bytes: u64,
    /// The `multi-pack-index` generation its repack wrote, if any.
    pub midx_gen: Option<u64>,
}

/// What one request sees of a repository, read in one database snapshot
/// (ADR 0004 item 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Every ref and its target, ordered by name.
    pub refs: Vec<(String, Oid)>,
    /// The committed packs, ordered by creation then id.
    pub packs: Vec<Pack>,
    /// The repository's full-repack generation, which a commit must still
    /// match (ADR 0004 item 3).
    pub full_repack_gen: u64,
}

/// One ref command of a commit transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefUpdate {
    /// The full ref name, such as `refs/heads/main`.
    pub name: String,
    /// The target the ref must have now; `None` creates it, so it must not
    /// exist.
    pub old: Option<Oid>,
    /// The target to set; `None` deletes the ref.
    pub new: Option<Oid>,
}

/// A commit transaction (ADR 0004 item 3), the one way every ref writer
/// changes refs.
#[derive(Debug, Clone, Copy)]
pub struct CommitTx<'a> {
    /// The repository.
    pub repo: RepoId,
    /// The ref commands, at least one, each naming a different ref.
    pub updates: &'a [RefUpdate],
    /// The writer's pack, which must be `Pending` and becomes `Committed`.
    pub pack: Option<PackId>,
    /// The full-repack generation of the writer's snapshot.
    pub full_repack_gen: u64,
    /// Who writes, for the ref log: the principal's subject.
    pub actor: &'a str,
    /// The request id, for the ref log.
    pub request: &'a str,
}

/// Why a commit transaction or a repack publish changed nothing.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// The named ref's target wasn't the expected one, or a ref to create
    /// exists: another writer moved it. Retry from a fresh snapshot.
    #[error("{0} moved concurrently; fetch and retry")]
    RefMismatch(String),
    /// The writer's pack, or a repack output, isn't `Pending` any more: GC
    /// expired it or it was already marked. Write a new pack and retry.
    #[error("the pack is no longer pending; retry with a new pack")]
    PackNotPending,
    /// The repack fence or the lease moved: a full repack published since
    /// the snapshot, the lease was lost, the ref log advanced past the
    /// repack's high-water mark, or an input is no longer committed. Retry
    /// from a fresh snapshot.
    #[error("the repository's pack set changed concurrently; retry from a fresh snapshot")]
    Fenced,
    /// The index failed, or the input is invalid.
    #[error(transparent)]
    Database(#[from] IndexError),
}

impl From<sqlx::Error> for CommitError {
    fn from(e: sqlx::Error) -> Self {
        Self::Database(IndexError::Database(e))
    }
}

/// A `ref_log` row (ADR 0004 item 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefLogRow {
    /// The row's sequence number, increasing across every repository.
    pub seq: u64,
    /// The repository.
    pub repo: RepoId,
    /// The full ref name.
    pub name: String,
    /// The target before; `None` for a create.
    pub old: Option<Oid>,
    /// The target after; `None` for a delete.
    pub new: Option<Oid>,
    /// Who wrote it.
    pub actor: String,
    /// The request id.
    pub request: String,
    /// When it committed, Unix seconds on the database clock.
    pub at: u64,
}

/// A repository's repack lease (ADR 0004 item 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    /// The repository.
    pub repo: RepoId,
    /// The holder named at acquisition.
    pub holder: String,
    /// Increases with every acquisition, so a holder that lost the lease
    /// can't publish.
    pub epoch: u64,
    /// When it expires, Unix milliseconds on the database clock.
    pub expires_at_ms: u64,
}

/// A repack's publish transaction (ADR 0004 item 5).
#[derive(Debug, Clone, Copy)]
pub struct RepackPublish<'a> {
    /// The repository.
    pub repo: RepoId,
    /// The leaseholder.
    pub holder: &'a str,
    /// The lease's epoch.
    pub epoch: u64,
    /// The new packs, each `Pending`, which become `Committed`.
    pub outputs: &'a [PackId],
    /// The replaced packs, each `Committed`, which become `Superseded`.
    pub inputs: &'a [PackId],
    /// For a full repack, the repository's highest `ref_log` sequence
    /// number when the working copy was taken: the publish fails if a ref
    /// changed since, and bumps the full-repack generation otherwise. `None`
    /// for a geometric repack, which drops nothing and takes neither fence.
    pub ref_log_high_water: Option<u64>,
}

/// What `reindex` could not place (ADR 0004 item 4, ruling R24).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnplacedKind {
    /// An unplaced commit tip.
    Commit(Oid),
    /// An unplaced annotated tag.
    Tag(Oid),
    /// An object in a committed pack that a recovered pack's objects
    /// reference directly.
    Edge(Oid),
    /// A recovered pack.
    Pack,
}

impl UnplacedKind {
    pub(crate) fn parts(self) -> (&'static str, Option<Oid>) {
        match self {
            Self::Commit(oid) => ("commit", Some(oid)),
            Self::Tag(oid) => ("tag", Some(oid)),
            Self::Edge(oid) => ("edge", Some(oid)),
            Self::Pack => ("pack", None),
        }
    }

    pub(crate) fn from_parts(kind: &str, oid: Option<Oid>) -> Result<Self, IndexError> {
        Ok(match (kind, oid) {
            ("commit", Some(oid)) => Self::Commit(oid),
            ("tag", Some(oid)) => Self::Tag(oid),
            ("edge", Some(oid)) => Self::Edge(oid),
            ("pack", None) => Self::Pack,
            (kind, oid) => {
                return Err(IndexError::Corrupt(format!(
                    "unplaced entry of kind {kind:?} with oid {oid:?}"
                )));
            }
        })
    }
}

/// A `reindex_unplaced` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Unplaced {
    /// The repository.
    pub repo: RepoId,
    /// What could not be placed.
    pub kind: UnplacedKind,
    /// The pack holding it, or the recovered pack itself.
    pub pack: PackId,
    /// When `reindex` found it, Unix seconds.
    pub found_at: u64,
}

/// Refs, packs and the repack lease of each repository.
#[async_trait::async_trait]
pub trait GitStore: Send + Sync + 'static {
    /// The repository's refs, committed packs and full-repack generation,
    /// read in one transaction.
    ///
    /// # Errors
    ///
    /// [`IndexError::UnknownRepo`], [`IndexError::Database`] or
    /// [`IndexError::Corrupt`].
    async fn snapshot(&self, repo: RepoId) -> Result<Snapshot, IndexError>;

    /// Records a pack before its files are uploaded. `pack.state` must be
    /// [`PackState::Pending`].
    ///
    /// # Errors
    ///
    /// [`IndexError::UnknownRepo`], [`IndexError::InvalidInput`] for
    /// another state or an id already recorded, and
    /// [`IndexError::Database`].
    async fn insert_pack(&self, pack: &Pack) -> Result<(), IndexError>;

    /// Runs the commit transaction of ADR 0004 item 3, atomically: the
    /// full-repack generation still matches, every ref command's
    /// compare-and-swap matches, the pack goes `Pending` to `Committed`, and
    /// one `ref_log` row per command is written. Otherwise nothing changes.
    /// On failure the caller marks its pack `Lost` with
    /// [`GitStore::mark_pack`].
    ///
    /// # Errors
    ///
    /// [`CommitError::Fenced`], [`CommitError::RefMismatch`] naming the
    /// first ref that didn't match, [`CommitError::PackNotPending`], and
    /// [`CommitError::Database`] for an unknown repository, invalid
    /// commands or a failed write.
    async fn commit(&self, tx: CommitTx<'_>) -> Result<(), CommitError>;

    /// Moves pack `id` from `from` to `to` if it is in state `from`, and
    /// reports whether it was. Only the transitions outside a commit or
    /// publish transaction are allowed: `Pending` to `Lost` or `Expired`,
    /// and `Recovered` to `Lost` (ADR 0004 items 3 to 5).
    ///
    /// # Errors
    ///
    /// [`IndexError::InvalidInput`] for any other transition, and
    /// [`IndexError::Database`].
    async fn mark_pack(
        &self,
        id: PackId,
        from: PackState,
        to: PackState,
    ) -> Result<bool, IndexError>;

    /// The repository's `ref_log` rows with a sequence number above
    /// `since_seq`, in order.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn ref_log_since(
        &self,
        repo: RepoId,
        since_seq: u64,
    ) -> Result<Vec<RefLogRow>, IndexError>;

    /// Takes the repository's repack lease for `ttl` on the database clock,
    /// if it is free, expired or already `holder`'s. Every acquisition
    /// increases the epoch. `None` if another holder has it.
    ///
    /// # Errors
    ///
    /// [`IndexError::UnknownRepo`], [`IndexError::InvalidInput`] for a
    /// `ttl` too large to store, and [`IndexError::Database`].
    async fn acquire_lease(
        &self,
        repo: RepoId,
        holder: &str,
        ttl: Duration,
    ) -> Result<Option<Lease>, IndexError>;

    /// Publishes a repack in one transaction (ADR 0004 item 5): the lease
    /// still holds, every output goes `Pending` to `Committed`, every input
    /// `Committed` to `Superseded`, and for a full repack the ref log hasn't
    /// advanced and the full-repack generation increases. Otherwise nothing
    /// changes; the caller marks the outputs `Lost`.
    ///
    /// # Errors
    ///
    /// [`CommitError::Fenced`], [`CommitError::PackNotPending`], and
    /// [`CommitError::Database`].
    async fn publish_repack(&self, p: RepackPublish<'_>) -> Result<(), CommitError>;

    /// The repository's unplaced entries, oldest first.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn unplaced(&self, repo: RepoId) -> Result<Vec<Unplaced>, IndexError>;

    /// Records unplaced entries; one already recorded is kept as it is.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`]; nothing is recorded.
    async fn record_unplaced(&self, rows: &[Unplaced]) -> Result<(), IndexError>;

    /// Deletes the repository's commit, tag and edge entries whose object is
    /// in `covered`, as a restore does for what its new ref reaches, and
    /// returns how many.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`].
    async fn clear_unplaced(&self, repo: RepoId, covered: &[Oid]) -> Result<u64, IndexError>;
}
