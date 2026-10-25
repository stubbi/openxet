//! The repack lease (ADR 0004 item 5): repacks of a repository run on one
//! leaseholder at a time. Every acquisition increases the lease's epoch,
//! and every repack publish requires the epoch, the holder and an unexpired
//! lease on the database clock, so a holder that lost the lease, even one
//! paused mid-publish, can't publish.

use std::time::Duration;

use openxet_index::{Lease, RepoId};

use crate::{Engine, GitError};

/// How long a repack lease lasts unless renewed: 10 minutes. A repack that
/// outlives its lease publishes nothing (its outputs end `lost`), so the
/// lease must cover the slowest repack the node runs.
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
