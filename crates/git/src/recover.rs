//! The operator's restore and discard of what `reindex` couldn't place
//! (ADR 0004 item 4, ruling R24). Like `reindex` itself, each runs with
//! the repository's writes stopped.

use std::collections::HashSet;

use openxet_index::{
    CommitError, CommitTx, Oid, Pack, PackId, PackState, RefUpdate, RepoId, Unplaced, UnplacedKind,
};

use crate::receive::valid_ref_name;
use crate::reindex::{
    REINDEX_ACTOR, closure_beyond_refs, closure_complete, first_oids, object_kind, pack_objects,
    reachable_objects,
};
use crate::repack::{git_out, present};
use crate::thin::oid_lines;
use crate::{Engine, GitError, SnapshotRepo};

/// How `reindex discard` names what it releases from committed packs (ADR
/// 0004 item 4); the operator docs use the same words.
pub const DISCARD_COMMITTED_WARNING: &str =
    "deleted at the next full repack, with no grace period; this cannot be undone";

/// What `reindex discard` released, or would with `dry_run`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscardReport {
    /// Whether nothing was changed.
    pub dry_run: bool,
    /// The recovered packs moved to `lost`, by name with their sizes: GC
    /// deletes them after the grace period.
    pub recovered_packs: Vec<(String, u64)>,
    /// The unplaced entries removed.
    pub entries: Vec<Unplaced>,
    /// How many objects in committed packs only the removed entries kept:
    /// [`DISCARD_COMMITTED_WARNING`]. An upper bound: some of them may also
    /// be held by a keep ref of the `ref_log` window (ADR 0004 item 5),
    /// and stay until that window passes.
    pub committed_objects: u64,
    /// Their size on disk.
    pub committed_bytes: u64,
}

/// Makes the unplaced commit or annotated tag `oid` the new ref `refname`
/// (ADR 0004 item 4): checks its closure against the committed packs plus
/// the recovered packs that hold the rest of it, then one commit
/// transaction creates the ref, moves those packs to `committed`, and
/// clears the entries the ref now covers.
///
/// # Errors
///
/// [`GitError::Restore`] if `refname` isn't a valid new ref, `oid` isn't a
/// commit or annotated tag the repository holds with a complete closure, a
/// branch would point at a tag, or the repository changed meanwhile; and
/// [`GitError::Index`], [`GitError::GitFailed`], [`GitError::Io`].
pub async fn reindex_restore(
    engine: &Engine,
    repo: RepoId,
    oid: Oid,
    refname: &str,
) -> Result<(), GitError> {
    let refuse = |reason: &str| GitError::Restore {
        oid,
        refname: refname.to_owned(),
        reason: reason.to_owned(),
    };
    if !valid_ref_name(refname) {
        return Err(refuse(
            "it isn't a valid ref name; use a full name such as refs/heads/restored",
        ));
    }
    let db = engine.index.snapshot(repo).await?;
    if db.refs.iter().any(|(n, _)| n == refname) {
        return Err(refuse("that ref exists; choose a new name"));
    }
    let recovered: Vec<Pack> = engine
        .index
        .packs(repo)
        .await?
        .into_iter()
        .map(|r| r.pack)
        .filter(|p| p.state == PackState::Recovered)
        .collect();
    let mut all = db.packs.clone();
    all.extend(recovered.iter().cloned());
    let wide = engine
        .snapshot_of(repo, db.refs.clone(), all, db.full_repack_gen, false)
        .await?;
    let kind = object_kind(engine, &wide, oid).await?;
    match kind.as_deref() {
        Some("commit") => {}
        Some("tag") if !refname.starts_with("refs/heads/") => {}
        Some("tag") => return Err(refuse("a branch must point at a commit, not a tag")),
        Some(_) | None => {
            return Err(refuse(
                "the repository holds no commit or annotated tag with that id",
            ));
        }
    }
    let new_objects = closure_beyond_refs(&wide, oid)
        .await?
        .ok_or_else(|| refuse("its closure is incomplete in the repository's packs"))?;
    let needed = needed_packs(&wide, &db.packs, &recovered, &new_objects).await?;
    let covered = covered_entries(engine, &wide, oid).await?;
    drop(wide);

    // The connectivity check of ADR 0004 item 4, on exactly the packs the
    // transaction commits.
    let mut packs = db.packs.clone();
    packs.extend(needed.iter().cloned());
    let narrow = engine
        .snapshot_of(repo, db.refs.clone(), packs, db.full_repack_gen, false)
        .await?;
    if !closure_complete(engine, &narrow, oid).await? {
        return Err(refuse(
            "its closure is incomplete in the repository's packs",
        ));
    }
    drop(narrow);

    let request = uuid::Uuid::new_v4().simple().to_string();
    let updates = [RefUpdate {
        name: refname.to_owned(),
        old: None,
        new: Some(oid),
    }];
    let ids: Vec<PackId> = needed.iter().map(|p| p.id).collect();
    match engine
        .index
        .restore(
            CommitTx {
                repo,
                updates: &updates,
                pack: None,
                full_repack_gen: db.full_repack_gen,
                actor: REINDEX_ACTOR,
                request: &request,
            },
            &ids,
            &covered,
        )
        .await
    {
        Ok(()) => Ok(()),
        Err(CommitError::Database(e)) => Err(e.into()),
        Err(CommitError::Fenced | CommitError::RefMismatch(_) | CommitError::PackNotPending) => {
            Err(refuse(
                "the repository changed while it ran; run the restore again",
            ))
        }
    }
}

/// The packs of `recovered` holding an object of `new_objects` that no
/// pack of `committed` holds.
async fn needed_packs(
    snap: &SnapshotRepo,
    committed: &[Pack],
    recovered: &[Pack],
    new_objects: &HashSet<Oid>,
) -> Result<Vec<Pack>, GitError> {
    let mut committed_objects = HashSet::new();
    for p in committed {
        committed_objects.extend(pack_objects(snap, &p.name).await?);
    }
    let missing: HashSet<&Oid> = new_objects
        .iter()
        .filter(|o| !committed_objects.contains(*o))
        .collect();
    let mut needed = Vec::new();
    for p in recovered {
        let objects = pack_objects(snap, &p.name).await?;
        if objects.iter().any(|o| missing.contains(o)) {
            needed.push(p.clone());
        }
    }
    Ok(needed)
}

/// The objects of the repository's commit, tag and edge entries that `oid`
/// reaches: what a ref to it covers.
async fn covered_entries(
    engine: &Engine,
    snap: &SnapshotRepo,
    oid: Oid,
) -> Result<Vec<Oid>, GitError> {
    let reached = reachable_objects(engine, snap, &[oid]).await?;
    Ok(engine
        .index
        .unplaced(snap.repo)
        .await?
        .into_iter()
        .filter_map(|u| match u.kind {
            UnplacedKind::Commit(o) | UnplacedKind::Tag(o) | UnplacedKind::Edge(o) => Some(o),
            UnplacedKind::Pack => None,
        })
        .filter(|o| reached.contains(o))
        .collect())
}

/// Ends recovery for `repo` (ADR 0004 item 4): in one index transaction
/// its recovered packs go `lost`, for GC to delete after the grace period,
/// and every unplaced entry is removed, so unplaced objects in committed packs lose their keep
/// roots: [`DISCARD_COMMITTED_WARNING`]. With `dry_run`, only reports.
///
/// # Errors
///
/// [`GitError::Index`], [`GitError::GitFailed`] and [`GitError::Io`].
pub async fn reindex_discard(
    engine: &Engine,
    repo: RepoId,
    dry_run: bool,
) -> Result<DiscardReport, GitError> {
    let entries = engine.index.unplaced(repo).await?;
    let recovered: Vec<Pack> = engine
        .index
        .packs(repo)
        .await?
        .into_iter()
        .map(|r| r.pack)
        .filter(|p| p.state == PackState::Recovered)
        .collect();
    let snap = engine.snapshot(repo).await?;
    let roots: Vec<Oid> = entries
        .iter()
        .filter_map(|u| match u.kind {
            UnplacedKind::Commit(o) | UnplacedKind::Tag(o) | UnplacedKind::Edge(o) => Some(o),
            UnplacedKind::Pack => None,
        })
        .collect();
    let roots = present(engine, &snap, &roots).await?;
    let (committed_objects, committed_bytes) = released(engine, &snap, &roots).await?;
    drop(snap);
    if !dry_run {
        engine.index.discard(repo).await?;
    }
    Ok(DiscardReport {
        dry_run,
        recovered_packs: recovered
            .iter()
            .map(|p| (p.name.clone(), p.bytes))
            .collect(),
        entries,
        committed_objects,
        committed_bytes,
    })
}

/// How many objects reachable from `roots` but from no ref the committed
/// packs hold, and their size on disk.
async fn released(
    engine: &Engine,
    snap: &SnapshotRepo,
    roots: &[Oid],
) -> Result<(u64, u64), GitError> {
    if roots.is_empty() {
        return Ok((0, 0));
    }
    let mut input = oid_lines("", roots);
    input.push_str(&oid_lines("^", snap.refs.iter().map(|(_, o)| *o)));
    let listed = git_out(
        engine,
        snap,
        &["rev-list", "--objects", "--stdin"],
        input.into_bytes(),
    )
    .await?;
    let objects: Vec<Oid> = first_oids(&listed).into_iter().collect();
    if objects.is_empty() {
        return Ok((0, 0));
    }
    let sizes = git_out(
        engine,
        snap,
        &["cat-file", "--batch-check=%(objectsize:disk)"],
        oid_lines("", &objects).into_bytes(),
    )
    .await?;
    let bytes = String::from_utf8_lossy(&sizes)
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .sum();
    Ok((objects.len() as u64, bytes))
}
