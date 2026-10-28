//! Registering worktrees made without OpenXet (by `git worktree add` in a
//! shell, Cursor's or Codex's setup, or a Claude Code subagent whose remove
//! hook never runs), so the idle TTL and the free-space floor cover them.
//!
//! The main worktree is never adopted: it is the human's checkout, not an
//! agent's workspace, so it is never evicted and `remove` never deletes it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::git::{current_branch, list_worktrees, worktree_root};
use crate::{ProjectId, Registry, Workspace, WorkspaceError, WorkspaceId, main_worktree};

/// What [`adopt`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Adoption {
    /// The worktree is now registered, or with `dry_run` would be.
    Adopted(Workspace),
    /// It was registered already, by `create` or an earlier adoption.
    Already(Workspace),
}

/// Why [`adopt_scan`] left a worktree out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanSkip {
    /// The repository's main worktree, or a bare repository's Git
    /// directory.
    MainWorktree,
    /// Git lists it, but its directory is gone (`git worktree prune` drops
    /// it).
    Missing,
}

impl ScanSkip {
    /// The stable name of this reason, as `--json` reports it.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::MainWorktree => "main_worktree",
            Self::Missing => "missing",
        }
    }
}

/// What [`adopt_scan`] did with each worktree of a repository.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    /// Newly registered, or with `dry_run` would be.
    pub adopted: Vec<PathBuf>,
    /// Registered already.
    pub already: Vec<PathBuf>,
    /// Left out, and why.
    pub skipped: Vec<(PathBuf, ScanSkip)>,
}

/// Registers the worktree `dir` is in as an adopted workspace, on the
/// branch it has checked out. Idempotent: a registered worktree is
/// returned as [`Adoption::Already`]. It counts as active from now, so the
/// idle TTL starts at its adoption.
///
/// # Errors
///
/// [`WorkspaceError::NotARepository`] if `dir` is in no worktree,
/// [`WorkspaceError::MainWorktree`] if it is in the main worktree,
/// [`WorkspaceError::Git`] if Git fails, and [`WorkspaceError::Database`]
/// if the registry can't be read or written.
pub async fn adopt(
    registry: &Registry,
    dir: &Path,
    dry_run: bool,
) -> Result<Adoption, WorkspaceError> {
    let root = canonical(&worktree_root(dir)?);
    if canonical(&main_worktree(&root)?) == root {
        return Err(WorkspaceError::MainWorktree(root));
    }
    let project = ProjectId::discover(&root)?;
    if let Some(workspace) = registry.get_by_path(&root).await? {
        return Ok(Adoption::Already(workspace));
    }
    let now = SystemTime::now();
    let workspace = Workspace {
        id: WorkspaceId::generate(),
        project,
        branch: current_branch(&root)?,
        path: root,
        created_at: now,
        last_active: now,
        adopted: true,
    };
    if dry_run {
        return Ok(Adoption::Adopted(workspace));
    }
    if let Err(err) = registry.insert(&workspace).await {
        // Another process adopted or created it since the lookup; the path
        // is unique in the registry.
        return match registry.get_by_path(&workspace.path).await? {
            Some(existing) => Ok(Adoption::Already(existing)),
            None => Err(err),
        };
    }
    Ok(Adoption::Adopted(workspace))
}

/// [`adopt`]s every linked worktree of the repository `repo` is in that
/// still exists, from `git worktree list`.
///
/// # Errors
///
/// As [`adopt`], for the repository and for any worktree in it.
pub async fn adopt_scan(
    registry: &Registry,
    repo: &Path,
    dry_run: bool,
) -> Result<ScanReport, WorkspaceError> {
    // Fails as `adopt` does outside a repository, before listing.
    ProjectId::discover(repo)?;
    let mut report = ScanReport::default();
    for (index, listed) in list_worktrees(repo)?.into_iter().enumerate() {
        let path = canonical(&listed.path);
        if index == 0 || listed.bare {
            report.skipped.push((path, ScanSkip::MainWorktree));
        } else if listed.prunable || !path.is_dir() {
            report.skipped.push((path, ScanSkip::Missing));
        } else {
            match adopt(registry, &path, dry_run).await? {
                Adoption::Adopted(ws) => report.adopted.push(ws.path),
                Adoption::Already(ws) => report.already.push(ws.path),
            }
        }
    }
    Ok(report)
}

/// The registry compares paths byte for byte and `create` registers
/// canonical ones; a path that can't be resolved stays as given.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}
