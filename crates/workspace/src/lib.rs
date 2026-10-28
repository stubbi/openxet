//! OpenXet agent workspaces (spec 2026-10-27): Git worktrees registered in a
//! per-user [`Registry`], whose reconstructible directories ([`Presets`],
//! [`reconstructible_dirs`]) are deduplicated by copy-on-write clones and
//! evicted under a disk budget, while durable work is never touched.
//!
//! Client code, local to one machine; it never depends on `xet-core`'s
//! client crates (ADR 0006). Unix only: it stores paths as their raw bytes
//! and relies on APFS, Btrfs and XFS clones.

#![forbid(unsafe_code)]

mod classify;
mod dedup;
mod error;
mod git;
mod paths;
mod registry;
mod seed;
mod space;

pub use classify::{Class, PROJECT_CONFIG, Presets, classify, reconstructible_dirs};
pub use dedup::{DedupOutcome, DedupReport, MIN_DEDUP_SIZE, dedup_file, dedup_tree};
pub use error::WorkspaceError;
pub use git::{
    UnsavedWork, add_worktree, branch_exists, delete_branch, main_worktree, remove_worktree,
    resolve_commit, unsaved_work,
};
pub use paths::{ProjectId, STATE_DIR_ENV, state_dir};
pub use registry::{Registry, Workspace, WorkspaceId};
pub use seed::{
    SeedReport, SeedSkip, nearest_sibling, reflink_supported, seed, seed_with, seedable_bytes,
};
pub use space::{IDLE_AFTER, Volume, allocated_bytes, default_floor, is_idle, volume};
