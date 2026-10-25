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
    /// The age of its objects, Unix seconds: when it was published, or for
    /// a repack's output its oldest input's. Nodes pin the pack's mtime to
    /// it (ADR 0004 item 4), so cruft expiry reads it; the pack sweep ages
    /// rows by [`PackRecord::inserted_at`] instead.
    pub created_at: u64,
    /// The size of its `.pack` file.
    pub bytes: u64,
    /// The `multi-pack-index` generation its repack wrote, if any.
    pub midx_gen: Option<u64>,
}

/// A pack row with the times the pack sweep reads (ADR 0004 item 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRecord {
    /// The row.
    pub pack: Pack,
    /// When the row was inserted, or a discard made it `lost`, Unix seconds
    /// on the database clock: what a `pending`, `lost` or `expired` row's
    /// grace period runs from.
    pub inserted_at: u64,
    /// When a commit transaction or a repack publish committed it, Unix
    /// seconds on the database clock; `None` if none did.
    pub committed_at: Option<u64>,
    /// When a repack superseded it, Unix seconds on the database clock.
    pub superseded_at: Option<u64>,
}

/// A pack deletion claim (ADR 0004 item 5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackDeletion {
    /// The pack's name.
    pub name: String,
    /// When the claim was taken, Unix seconds on the database clock.
    pub claimed_at: u64,
}

/// What `reindex` writes for one repository in place of its Git rows (ADR
/// 0004 item 4).
#[derive(Debug, Clone, Copy)]
pub struct Reindexed<'a> {
    /// The rebuilt refs, each name once.
    pub refs: &'a [(String, Oid)],
    /// The bucket ref log's rows, in increasing `seq`, kept with their
    /// sequence numbers: the bucket's records name them, so new rows must
    /// number after them.
    pub ref_log: &'a [RefLogRow],
    /// Ref log rows for what reindex itself changed, such as a ref rolled
    /// back to a value whose closure is complete; numbered after every
    /// other row.
    pub corrections: &'a [RefUpdate],
    /// Who made the corrections, for their rows.
    pub actor: &'a str,
    /// The request id of the corrections.
    pub request: &'a str,
    /// Every pack in the bucket, each [`PackState::Committed`] or
    /// [`PackState::Recovered`].
    pub packs: &'a [Pack],
    /// What could not be placed (ruling R24).
    pub unplaced: &'a [Unplaced],
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
    /// How long each acquisition or renewal lasts.
    pub ttl: Duration,
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

    /// Records a pack before its files are uploaded, stamping
    /// [`PackRecord::inserted_at`]. `pack.state` must be
    /// [`PackState::Pending`].
    ///
    /// # Errors
    ///
    /// [`IndexError::UnknownRepo`], [`IndexError::InvalidInput`] for
    /// another state or an id already recorded,
    /// [`IndexError::PackDeleting`] while the pack sweep deletes files of
    /// the same name (retry), and [`IndexError::Database`].
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

    /// Extends `holder`'s lease of epoch `epoch` to `ttl` from now on the
    /// database clock, keeping the epoch, if it still holds it unexpired: a
    /// leaseholder renews every `ttl / 3` while it works, so a long repack
    /// doesn't fence itself. `None` if the lease is lost.
    ///
    /// # Errors
    ///
    /// [`IndexError::InvalidInput`] for a `ttl` too large to store, and
    /// [`IndexError::Database`].
    async fn renew_lease(
        &self,
        repo: RepoId,
        holder: &str,
        epoch: u64,
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
    /// [`CommitError::Database`], which carries
    /// [`IndexError::InvalidInput`] for a full repack without outputs.
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

    /// Every pack row of the repository, whatever its state, ordered by
    /// creation then id: what the pack sweep and `reindex` read.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn packs(&self, repo: RepoId) -> Result<Vec<PackRecord>, IndexError>;

    /// Claims the deletion of pack `name`'s files (ADR 0004 item 5), in a
    /// transaction that checks no `pending`, `committed` or `recovered` row
    /// of the repository names it, and reports whether it did. While the
    /// claim exists, [`GitStore::insert_pack`] refuses rows of that name
    /// with [`IndexError::PackDeleting`]. A claim already held is kept.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`].
    async fn claim_pack_deletion(&self, repo: RepoId, name: &str) -> Result<bool, IndexError>;

    /// The repository's pack deletion claims, ordered by name: those a
    /// sweep that died left, for the next sweep or maintenance pass to
    /// finish.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn pack_deletions(&self, repo: RepoId) -> Result<Vec<PackDeletion>, IndexError>;

    /// Removes the claim on `name`, once its files are gone.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`].
    async fn release_pack_deletion(&self, repo: RepoId, name: &str) -> Result<(), IndexError>;

    /// Deletes pack row `id` if it is in `state`, and reports whether it
    /// was: the pack sweep's last step, once the pack's files are gone.
    /// Only `Superseded`, `Lost` and `Expired` rows are ever deleted (ADR
    /// 0004 item 5).
    ///
    /// # Errors
    ///
    /// [`IndexError::InvalidInput`] for any other state, and
    /// [`IndexError::Database`].
    async fn delete_pack(&self, id: PackId, state: PackState) -> Result<bool, IndexError>;

    /// The repository's highest `ref_log` sequence number, 0 if it has
    /// none: a repack's high-water mark (ADR 0004 item 5).
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`] or [`IndexError::Corrupt`].
    async fn ref_log_head(&self, repo: RepoId) -> Result<u64, IndexError>;

    /// Deletes the repository's `ref_log` rows committed before `before`
    /// (Unix seconds) with a sequence number of at most `through_seq`, and
    /// returns how many. The caller keeps the retention of ADR 0004 item 3
    /// and passes the highest row the bucket ref log already holds.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`].
    async fn prune_ref_log(
        &self,
        repo: RepoId,
        before: u64,
        through_seq: u64,
    ) -> Result<u64, IndexError>;

    /// The commit transaction of a `reindex` restore (ADR 0004 item 4):
    /// [`GitStore::commit`]'s checks and ref log rows, with no writer pack,
    /// plus every pack of `recovered` going `Recovered` to `Committed`, and
    /// the repository's commit, tag and edge entries whose object is in
    /// `covered`, and the pack entries of `recovered`, deleted. Otherwise
    /// nothing changes.
    ///
    /// # Errors
    ///
    /// [`CommitError::Fenced`] also when a pack of `recovered` isn't
    /// recovered any more, and the errors of [`GitStore::commit`];
    /// [`CommitError::Database`] carries [`IndexError::InvalidInput`] if
    /// `tx.pack` is set.
    async fn restore(
        &self,
        tx: CommitTx<'_>,
        recovered: &[PackId],
        covered: &[Oid],
    ) -> Result<(), CommitError>;

    /// `reindex discard` (ADR 0004 item 4) in one transaction: every
    /// `Recovered` pack of the repository goes `Lost`, its
    /// [`PackRecord::inserted_at`] stamped now so GC's grace period runs from
    /// the discard, and every unplaced entry is deleted. Returns how many packs and entries.
    ///
    /// # Errors
    ///
    /// [`IndexError::Database`]; nothing changes.
    async fn discard(&self, repo: RepoId) -> Result<(u64, u64), IndexError>;

    /// Replaces the repository's refs, `ref_log` rows, pack rows and
    /// unplaced entries with `r`, and drops its pack deletion claims (the
    /// bucket was just listed), in one transaction, and increases its
    /// full-repack generation, which fences any writer whose snapshot
    /// predates it (ADR 0004 item 4).
    ///
    /// # Errors
    ///
    /// [`IndexError::UnknownRepo`], [`IndexError::InvalidInput`] for a pack
    /// in another state or a ref log sequence number another repository
    /// holds, and [`IndexError::Database`]. Nothing changes on error.
    async fn reindex(&self, repo: RepoId, r: Reindexed<'_>) -> Result<(), IndexError>;
}
