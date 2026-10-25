//! The pack sweep, GC's Git half (ADR 0004 item 5, design §5.6):
//!
//! - `pending` packs older than the grace period are marked `expired`
//!   first, with `WHERE state = 'pending'`, so no late commit can use them;
//! - `superseded` packs whose grace has passed since they were superseded,
//!   and `lost` and `expired` packs past their grace, are deleted with every
//!   side file, then their rows;
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
//! finishes. A pack name is a checksum of its contents, so two rows can
//! name the same files (a repack that rewrites a pack as it was, a pushed
//! pack sent again): files go only once no remaining row names them.

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use openxet_index::{PackRecord, PackState, RepoId};
use openxet_storage::{Key, MidxExt, PackExt, PackName};

use crate::reflog::{derived_head, repo_hex};
use crate::{Engine, GitError, check_grace};

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
    /// The multi-pack-index generations deleted.
    pub midx_deleted: Vec<u64>,
    /// How many `ref_log` rows were pruned.
    pub ref_log_pruned: u64,
}

/// Sweeps `repo`'s packs as of `now` (Unix seconds) with `grace`.
///
/// # Errors
///
/// [`GitError::GraceTooShort`] below [`crate::MIN_GRACE`],
/// [`GitError::Index`] and [`GitError::Storage`]; what was deleted before
/// an error stays deleted, and the next sweep finishes the rest.
pub async fn sweep_packs(
    engine: &Engine,
    repo: RepoId,
    grace: Duration,
    now: u64,
    dry_run: bool,
) -> Result<SweepReport, GitError> {
    check_grace(grace)?;
    let grace_s = grace.as_secs();
    let due = |since: u64| since.saturating_add(grace_s) <= now;
    let mut rows = engine.index.packs(repo).await?;
    let mut report = SweepReport {
        dry_run,
        ..SweepReport::default()
    };

    for row in &mut rows {
        if row.pack.state == PackState::Pending && due(row.pack.created_at) {
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

    let gone: Vec<&PackRecord> = rows
        .iter()
        .filter(|r| match r.pack.state {
            PackState::Lost | PackState::Expired => due(r.pack.created_at),
            PackState::Superseded => due(r.superseded_at.unwrap_or(r.pack.created_at)),
            PackState::Pending | PackState::Committed | PackState::Recovered => false,
        })
        .collect();
    let gone_ids: HashSet<_> = gone.iter().map(|r| r.pack.id).collect();
    let named: HashSet<&str> = rows
        .iter()
        .filter(|r| !gone_ids.contains(&r.pack.id))
        .map(|r| r.pack.name.as_str())
        .collect();
    let repo_hex = repo_hex(repo)?;

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

    let mut files_deleted = HashSet::new();
    for row in gone {
        let name = &row.pack.name;
        if !dry_run {
            if !named.contains(name.as_str()) && files_deleted.insert(name.clone()) {
                let pack_name = PackName::parse(name)?;
                for ext in [
                    PackExt::Pack,
                    PackExt::Idx,
                    PackExt::Rev,
                    PackExt::Bitmap,
                    PackExt::Mtimes,
                ] {
                    let key = Key::Pack {
                        repo: repo_hex,
                        name: pack_name.clone(),
                        ext,
                    };
                    engine.bucket.delete(&key).await?;
                }
            }
            if !engine
                .index
                .delete_pack(row.pack.id, row.pack.state)
                .await?
            {
                continue;
            }
        }
        report.deleted.push(SweptPack {
            name: name.clone(),
            state: row.pack.state,
            bytes: row.pack.bytes,
        });
    }

    // Rows must outlive the grace period plus the longest a pack stays
    // pending, which is the grace period again (ADR 0004 item 3), and must
    // already be in the bucket ref log.
    let before = now.saturating_sub(grace_s.saturating_mul(2));
    let through = derived_head(engine, repo).await?;
    report.ref_log_pruned = if dry_run {
        let rows = engine.index.ref_log_since(repo, 0).await?;
        rows.iter()
            .filter(|r| r.at < before && r.seq <= through)
            .count() as u64
    } else {
        engine.index.prune_ref_log(repo, before, through).await?
    };
    Ok(report)
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
                        .map(|r| r.superseded_at.unwrap_or(r.pack.created_at))
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
