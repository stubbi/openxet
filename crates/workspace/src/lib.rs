//! OpenXet agent workspaces (spec 2026-10-27): Git worktrees registered in a
//! per-user [`Registry`], whose reconstructible directories ([`Presets`],
//! [`reconstructible_dirs`]) are deduplicated by copy-on-write clones and
//! evicted under a disk [`Budget`] by the [`run_daemon`] loop, while durable
//! work is never touched.
//!
//! Client code, local to one machine; it never depends on `xet-core`'s
//! client crates (ADR 0006). Unix only: it stores paths as their raw bytes
//! and relies on APFS, Btrfs and XFS clones.

#![forbid(unsafe_code)]

mod activity;
mod classify;
mod daemon;
mod dedup;
mod error;
mod evict;
mod git;
mod paths;
mod registry;
mod seed;
mod space;

pub use activity::active_workspaces;
pub use classify::{Class, PROJECT_CONFIG, Presets, classify, reconstructible_dirs};
pub use daemon::{DAEMON_LOCK, daemon_running, run_daemon};
pub use dedup::{DedupOutcome, DedupReport, MIN_DEDUP_SIZE, dedup_file, dedup_tree};
pub use error::WorkspaceError;
pub use evict::{evict_expired, eviction_order, free_to_floor};
pub use git::{
    UnsavedWork, add_worktree, branch_exists, delete_branch, main_worktree, remove_worktree,
    resolve_commit, unsaved_work,
};
pub use paths::{ProjectId, STATE_DIR_ENV, state_dir};
pub use registry::{Registry, Workspace, WorkspaceId};
pub use seed::{
    SETTLE_CAP, SeedReport, SeedSkip, SettleWait, nearest_sibling, reflink_supported, seed,
    seed_with, seedable_bytes, wait_until_settled,
};
pub use space::{
    Budget, IDLE_AFTER, Volume, allocated_bytes, default_floor, is_idle, user_config_path, volume,
};
