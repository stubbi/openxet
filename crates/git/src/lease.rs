//! The repack lease (ADR 0004 item 5): repacks of a repository run on one
//! leaseholder at a time. Every acquisition increases the lease's epoch,
//! and every repack publish requires the epoch, the holder and an unexpired
//! lease on the database clock, so a holder that lost the lease, even one
//! paused mid-publish, can't publish. The holder renews the lease every
//! third of its TTL while it works ([`while_renewing`]), under the same
//! epoch, so a repack longer than the TTL doesn't fence itself.

use std::future::Future;
use std::time::Duration;

use futures_util::future::{Either, select};
use openxet_index::{Lease, RepoId};

use crate::{Engine, GitError};

/// How long a repack lease lasts unless renewed: 10 minutes. The holder
/// renews it every third of that while it works, so this bounds how long a
/// holder that died keeps others waiting, not how long a repack may run.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_mins(10);

/// Takes `repo`'s repack lease for `holder` for `ttl`, if it is free,
/// expired or already `holder`'s; `None` if another holder has it.
/// Re-acquiring as the same holder renews it under a new epoch, which
/// fences that holder's own repacks still running under the old one.
///
/// # Errors
///
/// [`GitError::Index`], with [`openxet_index::IndexError::UnknownRepo`] for
/// a repository that doesn't exist.
pub async fn acquire_lease(
    engine: &Engine,
    repo: RepoId,
    holder: &str,
    ttl: Duration,
) -> Result<Option<Lease>, GitError> {
    Ok(engine.index.acquire_lease(repo, holder, ttl).await?)
}

/// Renews `lease`, failing with [`GitError::LeaseLost`] if it is gone: how
/// the sweep and `reindex` check they still exclude repacks before each
/// step that must not overlap one.
pub(crate) async fn renew(engine: &Engine, lease: &Lease) -> Result<(), GitError> {
    engine
        .index
        .renew_lease(lease.repo, &lease.holder, lease.epoch, lease.ttl)
        .await?
        .map(drop)
        .ok_or(GitError::LeaseLost)
}

/// Runs `work` while renewing `lease` every `lease.ttl / 3`. Once a renewal
/// finds the lease lost, renewing stops and `work` runs on: its publish is
/// fenced by the epoch anyway, and stopping it midway would only leave
/// `pending` rows for GC. A renewal the index fails is logged and retried
/// at the next tick. Returns `work`'s output and whether the lease was
/// still held at the last renewal.
pub(crate) async fn while_renewing<T>(
    engine: &Engine,
    lease: &Lease,
    work: impl Future<Output = T>,
) -> (T, bool) {
    let mut work = std::pin::pin!(work);
    let period = (lease.ttl / 3).max(Duration::from_millis(10));
    let mut held = true;
    loop {
        if !held {
            return (work.await, false);
        }
        let tick = std::pin::pin!(tokio::time::sleep(period));
        match select(work.as_mut(), tick).await {
            Either::Left((out, _)) => return (out, held),
            Either::Right(((), _)) => {
                match engine
                    .index
                    .renew_lease(lease.repo, &lease.holder, lease.epoch, lease.ttl)
                    .await
                {
                    Ok(Some(_)) => {}
                    Ok(None) => {
                        held = false;
                        tracing::warn!(repo = %lease.repo, holder = %lease.holder, "the repack lease was lost while its holder worked; what it publishes will be fenced");
                    }
                    Err(e) => {
                        tracing::warn!(repo = %lease.repo, error = %e, "renewing the repack lease failed; retrying at the next tick");
                    }
                }
            }
        }
    }
}
