//! The server's Git background task: every [`MAINTENANCE_INTERVAL`] it
//! derives the bucket ref log of every repository, which keeps it within
//! the documented RPO of 60 s (design §5.3); under the repack lease it
//! finishes the pack deletion claims a sweep that died left, which would
//! otherwise refuse pushes of an identical pack until the next sweep, and
//! runs a geometric repack on each repository with more than
//! [`GEOMETRIC_REPACK_PACKS`] committed packs (ADR 0004 item 5). Full
//! repacks, the pack sweep and `reindex` are operator commands (M4).

use std::sync::Arc;
use std::time::Duration;

use openxet_index::{RepoFilter, RepoId};

use crate::sweep::finish_claims;
use crate::{
    DEFAULT_LEASE_TTL, Engine, GitError, acquire_lease, derive_bucket_ref_log, repack_geometric,
};

/// How often the background task runs: 10 s, well inside the bucket ref
/// log's 60 s RPO.
pub const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(10);

/// A repository with more committed packs than this gets a geometric
/// repack.
pub const GEOMETRIC_REPACK_PACKS: usize = 50;

/// Repositories listed per page.
const PAGE: usize = 1000;

/// What one pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    /// How many repositories it visited.
    pub repos: usize,
    /// How many ref log rows it wrote to the bucket.
    pub ref_log_rows: u64,
    /// The repositories it repacked.
    pub repacked: Vec<RepoId>,
    /// How many stale pack deletion claims it finished.
    pub claims_finished: usize,
    /// How many repositories failed a step; each failure is logged and the
    /// next pass retries it.
    pub failures: usize,
}

/// One pass over every repository, as `holder` for the repack lease.
///
/// # Errors
///
/// [`GitError::Index`] if the repositories can't be listed; a failure on
/// one repository is logged and counted, and the pass goes on.
pub async fn maintain(engine: &Engine, holder: &str) -> Result<MaintenanceReport, GitError> {
    let mut report = MaintenanceReport::default();
    let mut cursor = None;
    loop {
        let page = engine
            .index
            .list_repos(RepoFilter::default(), cursor.as_deref(), PAGE)
            .await?;
        for repo in &page.items {
            report.repos += 1;
            if let Err(e) = maintain_repo(engine, repo.id, holder, &mut report).await {
                report.failures += 1;
                tracing::warn!(repo = %repo.id, error = %e, "Git maintenance of a repository failed; the next pass retries it");
            }
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => return Ok(report),
        }
    }
}

async fn maintain_repo(
    engine: &Engine,
    repo: RepoId,
    holder: &str,
    report: &mut MaintenanceReport,
) -> Result<(), GitError> {
    report.ref_log_rows += derive_bucket_ref_log(engine, repo).await?;
    let claims = !engine.index.pack_deletions(repo).await?.is_empty();
    let repack = engine.index.snapshot(repo).await?.packs.len() > GEOMETRIC_REPACK_PACKS;
    if !claims && !repack {
        return Ok(());
    }
    // Held by a sweep or a repack elsewhere: that holder finishes the
    // claims, or the next pass does.
    let Some(lease) = acquire_lease(engine, repo, holder, DEFAULT_LEASE_TTL).await? else {
        return Ok(());
    };
    if claims {
        report.claims_finished += finish_claims(engine, &lease).await?.len();
    }
    if repack
        && !repack_geometric(engine, repo, &lease)
            .await?
            .published
            .is_empty()
    {
        report.repacked.push(repo);
    }
    Ok(())
}

/// Runs [`maintain`] every [`MAINTENANCE_INTERVAL`] until the task is
/// dropped or aborted. `holder` names this node in repack leases and must
/// differ between nodes.
pub async fn run_maintenance(engine: Arc<Engine>, holder: String) {
    let mut ticks = tokio::time::interval(MAINTENANCE_INTERVAL);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        if let Err(e) = maintain(&engine, &holder).await {
            tracing::warn!(error = %e, "Git maintenance failed; it runs again in 10 s");
        }
    }
}
