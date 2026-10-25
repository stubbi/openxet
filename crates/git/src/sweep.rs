//! The pack sweep, GC's Git half (ADR 0004 item 5, design §5.6). It runs
//! under the repository's repack lease, so no repack publishes while it
//! decides:
//!
//! - `pending` packs inserted more than the grace period ago are marked
//!   `expired` first, with `WHERE state = 'pending'`, so no late commit can
//!   use them;
//! - `superseded` packs whose grace has passed since they were superseded,
//!   and `lost` and `expired` packs inserted more than the grace period
//!   ago, are deleted with every side file, then their rows. Rows are aged
//!   by when they were inserted, not by `created_at`, which is the age of
//!   their objects: a repack's outputs carry their oldest input's, and must
//!   not look abandoned the moment they are written;
//! - a multi-pack-index generation goes with its side files once it has not
//!   been current for the grace period: since a newer one was committed, or
//!   since the last pack carrying it left the committed set;
//! - `committed` and `recovered` packs are never touched, nor any bucket
//!   object no row describes;
//! - `ref_log` rows older than the grace period plus the longest a pack can
//!   stay pending (another grace period) are pruned once the bucket ref log
//!   holds them (ADR 0004 item 3).
//!
//! Superseded files stay in the bucket for the whole grace period, so
//! requests in flight and `reindex` can still read them. Files are deleted
//! before their rows, so a sweep that dies halfway leaves rows the next one
//! finishes.
//!
//! A pack name is a checksum of its contents, so two rows can name the same
//! files (a pushed pack sent again, a repack that rewrites a pack as it
//! was, a byte-identical repack output). Files go only once no remaining
//! row names them, and only under a deletion claim in the index: the claim
//! is taken only while no `pending`, `committed` or `recovered` row names
//! the pack, and while it exists no row of that name can be inserted (the
//! writer retries). A row inserted after the sweep read the rows therefore
//! either stops the claim or waits for the files to go and uploads them
//! again; it never commits over deleted files. A claim a dead sweep left is
//! finished by the next one.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use openxet_index::{Lease, PackRecord, PackState, RepoId};
use openxet_storage::{Key, MidxExt, PackExt, PackName, RepoIdHex};

use crate::lease::renew;
use crate::reflog::{derived_head, repo_hex};
use crate::{Engine, GitError, check_grace, unix_now};

/// How old a deletion claim must be before finishing it is logged as a
/// warning: one hour, far longer than a sweep takes to delete a pack.
pub(crate) const STALE_CLAIM: Duration = Duration::from_hours(1);

/// Every file a pack can have.
const PACK_FILES: [PackExt; 5] = [
    PackExt::Pack,
    PackExt::Idx,
    PackExt::Rev,
    PackExt::Bitmap,
    PackExt::Mtimes,
];

/// A pack the sweep deleted, or would delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweptPack {
    /// The pack's name.
    pub name: String,
    /// Its state when it went.
    pub state: PackState,
    /// The size of its `.pack`.
    pub bytes: u64,
}

/// What a sweep did, or would do with `dry_run`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Whether nothing was changed.
    pub dry_run: bool,
    /// The pending packs marked `expired`.
    pub expired: Vec<String>,
    /// The packs deleted with their side files and rows.
    pub deleted: Vec<SweptPack>,
    /// The deletion claims an interrupted sweep left, now finished: the
    /// pack's files deleted, or kept if a row names them again.
    pub finished_claims: Vec<String>,
    /// The multi-pack-index generations deleted.
    pub midx_deleted: Vec<u64>,
    /// How many `ref_log` rows were pruned.
    pub ref_log_pruned: u64,
}

/// Sweeps `repo`'s packs as of `now` (Unix seconds) with `grace`, holding
/// `lease`, the repository's repack lease.
///
/// # Errors
///
/// [`GitError::GraceTooShort`] below [`crate::MIN_GRACE`],
/// [`GitError::LeaseLost`] if `lease` isn't held (checked before every
/// deletion claim), [`GitError::Index`] and [`GitError::Storage`]; what was
/// deleted before an error stays deleted, and the next sweep finishes the
/// rest.
pub async fn sweep_packs(
    engine: &Engine,
    repo: RepoId,
    lease: &Lease,
    grace: Duration,
    now: u64,
    dry_run: bool,
) -> Result<SweepReport, GitError> {
    check_grace(grace)?;
    if lease.repo != repo {
        return Err(GitError::LeaseLost);
    }
    renew(engine, lease).await?;
    let grace_s = grace.as_secs();
    let due = |since: u64| since.saturating_add(grace_s) <= now;
    let repo_hex = repo_hex(repo)?;
    let mut report = SweepReport {
        dry_run,
        ..SweepReport::default()
    };
    if !dry_run {
        report.finished_claims = finish_claims(engine, lease).await?;
    }

    let mut rows = engine.index.packs(repo).await?;
    for row in &mut rows {
        if row.pack.state == PackState::Pending && due(row.inserted_at) {
            let expired = dry_run
                || engine
                    .index
                    .mark_pack(row.pack.id, PackState::Pending, PackState::Expired)
                    .await?;
            if expired {
                row.pack.state = PackState::Expired;
                report.expired.push(row.pack.name.clone());
            }
        }
    }
    #[cfg(feature = "fault-injection")]
    engine.fault(crate::FaultPoint::SweepAfterRead, "").await;

    let mut gone: BTreeMap<&str, Vec<&PackRecord>> = BTreeMap::new();
    for r in &rows {
        let past_grace = match r.pack.state {
            PackState::Lost | PackState::Expired => due(r.inserted_at),
            PackState::Superseded => due(r.superseded_at.unwrap_or(r.inserted_at)),
            PackState::Pending | PackState::Committed | PackState::Recovered => false,
        };
        if past_grace {
            gone.entry(r.pack.name.as_str()).or_default().push(r);
        }
    }
    let gone_ids: HashSet<_> = gone.values().flatten().map(|r| r.pack.id).collect();
    let named: HashSet<&str> = rows
        .iter()
        .filter(|r| !gone_ids.contains(&r.pack.id))
        .map(|r| r.pack.name.as_str())
        .collect();

    for generation in retired_midx(&rows, &due) {
        if !dry_run {
            for ext in [MidxExt::Bitmap, MidxExt::Rev, MidxExt::MultiPackIndex] {
                let key = Key::Midx {
                    repo: repo_hex,
                    generation,
                    ext,
                };
                engine.bucket.delete(&key).await?;
            }
        }
        report.midx_deleted.push(generation);
    }

    for (name, rows) in gone {
        let mut claimed = false;
        if !dry_run && !named.contains(name) {
            renew(engine, lease).await?;
            claimed = engine.index.claim_pack_deletion(repo, name).await?;
            if claimed {
                #[cfg(feature = "fault-injection")]
                engine.fault(crate::FaultPoint::SweepAfterClaim, name).await;
                delete_files(engine, repo_hex, name).await?;
            }
        }
        for row in rows {
            if !dry_run
                && !engine
                    .index
                    .delete_pack(row.pack.id, row.pack.state)
                    .await?
            {
                continue;
            }
            report.deleted.push(SweptPack {
                name: name.to_owned(),
                state: row.pack.state,
                bytes: row.pack.bytes,
            });
        }
        if claimed {
            engine.index.release_pack_deletion(repo, name).await?;
        }
    }

    // Rows must outlive the grace period plus the longest a pack stays
    // pending, which is the grace period again (ADR 0004 item 3).
    let before = now.saturating_sub(grace_s.saturating_mul(2));
    report.ref_log_pruned = prune_ref_log(engine, repo, before, dry_run).await?;
    Ok(report)
}

/// Finishes the deletion claims a sweep that died left, holding `lease`:
/// deletes a claimed pack's files if still no live row names it, then
/// removes the claim. Returns the names. The sweep and every maintenance
/// pass ([`crate::maintain`]) call it, so a dead sweep's claim blocks
/// pushes of an identical pack for one pass at most; a claim older than
/// [`STALE_CLAIM`] is logged, since only a sweep that died leaves one.
///
/// # Errors
///
/// [`GitError::LeaseLost`], [`GitError::Index`] and [`GitError::Storage`].
pub(crate) async fn finish_claims(engine: &Engine, lease: &Lease) -> Result<Vec<String>, GitError> {
    let repo = lease.repo;
    let repo_hex = repo_hex(repo)?;
    let now = unix_now();
    let mut finished = Vec::new();
    for claim in engine.index.pack_deletions(repo).await? {
        let name = claim.name;
        if claim.claimed_at.saturating_add(STALE_CLAIM.as_secs()) <= now {
            tracing::warn!(%repo, pack = %name, claimed_at = claim.claimed_at, "a pack deletion claim is over an hour old: a pack sweep died holding it; finishing it now");
        }
        renew(engine, lease).await?;
        if engine.index.claim_pack_deletion(repo, &name).await? {
            delete_files(engine, repo_hex, &name).await?;
        }
        engine.index.release_pack_deletion(repo, &name).await?;
        finished.push(name);
    }
    Ok(finished)
}

/// Prunes `repo`'s `ref_log` rows committed before `before` that the
/// bucket ref log already holds, or counts them with `dry_run`.
async fn prune_ref_log(
    engine: &Engine,
    repo: RepoId,
    before: u64,
    dry_run: bool,
) -> Result<u64, GitError> {
    let through = derived_head(engine, repo).await?;
    Ok(if dry_run {
        let rows = engine.index.ref_log_since(repo, 0).await?;
        rows.iter()
            .filter(|r| r.at < before && r.seq <= through)
            .count() as u64
    } else {
        engine.index.prune_ref_log(repo, before, through).await?
    })
}

/// Deletes every file of pack `name`.
async fn delete_files(engine: &Engine, repo: RepoIdHex, name: &str) -> Result<(), GitError> {
    let pack_name = PackName::parse(name)?;
    for ext in PACK_FILES {
        let key = Key::Pack {
            repo,
            name: pack_name.clone(),
            ext,
        };
        engine.bucket.delete(&key).await?;
    }
    Ok(())
}

/// The multi-pack-index generations that have not been current for the
/// grace period. A snapshot links the newest generation among its
/// committed packs, so a generation stops being current when a newer one
/// is committed, or when no committed pack carries it any more; a
/// generation whose publish failed never was.
fn retired_midx(rows: &[PackRecord], due: &impl Fn(u64) -> bool) -> Vec<u64> {
    let current = rows
        .iter()
        .filter(|r| r.pack.state == PackState::Committed)
        .filter_map(|r| r.pack.midx_gen)
        .max();
    let generations: BTreeSet<u64> = rows.iter().filter_map(|r| r.pack.midx_gen).collect();
    generations
        .into_iter()
        .filter(|g| Some(*g) != current)
        .filter(|&g| {
            let newer_committed = rows
                .iter()
                .filter(|r| r.pack.midx_gen.is_some_and(|n| n > g))
                .filter_map(|r| r.committed_at)
                .min();
            let carriers: Vec<&PackRecord> =
                rows.iter().filter(|r| r.pack.midx_gen == Some(g)).collect();
            let all_left = carriers.iter().all(|r| {
                matches!(
                    r.pack.state,
                    PackState::Superseded | PackState::Lost | PackState::Expired
                )
            });
            let last_left = all_left
                .then(|| {
                    carriers
                        .iter()
                        .map(|r| r.superseded_at.unwrap_or(r.inserted_at))
                        .max()
                })
                .flatten();
            [newer_committed, last_left]
                .into_iter()
                .flatten()
                .min()
                .is_some_and(due)
        })
        .collect()
}
