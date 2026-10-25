//! The Git role's settings (`[git]` in the server config) and their
//! defaults (ruling M2-R16, ADR 0004 items 2 and 3).

use std::path::PathBuf;
use std::time::Duration;

/// A per-request work budget. A check that runs out of it fails closed
/// (ADR 0004 items 2 and 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The most objects one request may visit: commits for the R22
    /// reachability walk, objects of any kind for the thin-base check.
    pub max_objects: u64,
    /// The most time one request may spend.
    pub max_time: Duration,
    /// The most requests that may run the check at once per repository;
    /// `None` for no limit. A request over it is refused as retryable.
    pub max_concurrent_per_repo: Option<usize>,
    /// The most requests that may run the check at once per principal;
    /// `None` for no limit.
    pub max_concurrent_per_principal: Option<usize>,
}

impl Budget {
    /// `git.reachability`'s defaults (ADR 0004 item 2).
    pub const REACHABILITY: Self = Self {
        max_objects: 200_000,
        max_time: Duration::from_secs(2),
        max_concurrent_per_repo: Some(2),
        max_concurrent_per_principal: Some(4),
    };

    /// `git.thin_bases`'s defaults (ADR 0004 item 3), placeholders until
    /// the M2 benchmark measures them (ruling M2-R16).
    pub const THIN_BASES: Self = Self {
        max_objects: 2_000_000,
        max_time: Duration::from_secs(2),
        max_concurrent_per_repo: None,
        max_concurrent_per_principal: None,
    };
}

/// `cache_max_bytes`'s default: 20 GiB (ruling M2-R16).
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 20 * 1024 * 1024 * 1024;

/// The Git role's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitConfig {
    /// The node cache: hydrated packs under `repos/`, request snapshots
    /// under `snapshots/`, openxet's hook directory `hooks/`. The server
    /// defaults it to `{data_dir}/git-cache`. One process owns it at a
    /// time; it must make hard links within itself (ADR 0004 item 4).
    pub cache_dir: PathBuf,
    /// The cache size eviction keeps hydrated packs under.
    pub cache_max_bytes: u64,
    /// The R22 reachability walk's budget.
    pub reachability: Budget,
    /// The thin-base check's budget.
    pub thin_bases: Budget,
    /// The largest blob a push may carry that isn't an LFS pointer; `None`
    /// accepts any size and only reports large ones.
    pub max_plain_blob_bytes: Option<u64>,
}

impl GitConfig {
    /// The defaults, with the node cache at `cache_dir`.
    #[must_use]
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            cache_max_bytes: DEFAULT_CACHE_MAX_BYTES,
            reachability: Budget::REACHABILITY,
            thin_bases: Budget::THIN_BASES,
            max_plain_blob_bytes: None,
        }
    }
}
