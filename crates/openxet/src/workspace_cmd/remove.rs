//! `openxet workspace remove`.

use std::path::{Path, PathBuf};

use openxet_workspace::{
    Registry, UnsavedWork, WorkspaceError, allocated_bytes, delete_branch, remove_worktree,
    unsaved_work,
};
use serde::Serialize;

use super::{Context, absolute, human_bytes, workspace_error};
use crate::error::{CliError, FileError};
use crate::output::HumanDisplay;

pub(super) const UNSAVED_HINT: &str = "Commit and push it, or pass `--discard` to delete it \
     with the workspace. Ignored files count as work unless they are under `target`, \
     `node_modules`, `.venv` or a path `.openxet/workspace.toml` lists as reconstructible.";

#[derive(Debug, clap::Args)]
pub struct RemoveArgs {
    /// The workspace's directory, as `openxet workspace create` printed it
    #[arg(value_name = "PATH")]
    path: PathBuf,
    /// Remove it even if it holds work that exists nowhere else, and lose
    /// that work
    #[arg(long)]
    discard: bool,
    /// Report what would be removed and freed, and change nothing
    #[arg(long)]
    dry_run: bool,
}

/// The `openxet.workspace.removed.v1` result.
#[derive(Debug, Serialize)]
pub struct Removed {
    schema: &'static str,
    path: String,
    /// What its files took on disk, or with `dry_run` take. Clones shared
    /// with other workspaces are counted in full, so at most this much is
    /// freed.
    freed_bytes: u64,
    /// The branch deleted with it, if any.
    deleted_branch: Option<String>,
    dry_run: bool,
}

impl HumanDisplay for Removed {
    fn human(&self) -> String {
        let branch = self
            .deleted_branch
            .as_deref()
            .map(|branch| format!(" and branch {branch}"))
            .unwrap_or_default();
        let size = human_bytes(self.freed_bytes);
        if self.dry_run {
            format!(
                "Dry run: would remove {}{branch}, freeing up to {size}. Nothing was changed.",
                self.path
            )
        } else {
            format!("Removed {}{branch}, freeing up to {size}.", self.path)
        }
    }
}

pub(super) async fn remove(args: &RemoveArgs, context: &Context) -> Result<Removed, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let path = absolute(context, &args.path);
    let path = path.canonicalize().unwrap_or(path);
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let workspace = registry
        .get_by_path(&path)
        .await
        .map_err(error)?
        .ok_or_else(|| error(WorkspaceError::NotAWorkspace(path.display().to_string())))?;
    let repo = PathBuf::from(workspace.project.as_str());
    let exists = path.exists();
    // A workspace OpenXet adopted may sit on a branch the human made; and
    // with its directory gone, there is no telling what the branch holds.
    let branch = workspace
        .branch
        .as_deref()
        .filter(|_| !workspace.adopted && exists);
    if exists && !args.discard {
        let items = unsaved_work(&path, branch).map_err(error)?;
        if !items.is_empty() {
            return Err(unsaved(&path, &items));
        }
    }
    let result = |freed_bytes, deleted_branch, dry_run| Removed {
        schema: "openxet.workspace.removed.v1",
        path: path.display().to_string(),
        freed_bytes,
        deleted_branch,
        dry_run,
    };
    let freed = allocated_bytes(&path);
    if args.dry_run {
        return Ok(result(freed, branch.map(str::to_owned), true));
    }
    registry.forget_canonical(&path).await.map_err(error)?;
    remove_worktree(&repo, &path).map_err(error)?;
    let deleted = match branch {
        Some(branch) if delete_branch(&repo, branch).map_err(error)? => Some(branch.to_owned()),
        _ => None,
    };
    registry.remove(&workspace.id).await.map_err(error)?;
    Ok(result(freed, deleted, false))
}

fn unsaved(path: &Path, items: &[UnsavedWork]) -> CliError {
    let unpushed: Vec<String> = items
        .iter()
        .filter(|item| matches!(item, UnsavedWork::Unpushed { .. }))
        .map(ToString::to_string)
        .collect();
    let mut message = format!(
        "{} holds work that exists nowhere else; nothing was removed",
        path.display()
    );
    if !unpushed.is_empty() {
        message = format!("{message} ({})", unpushed.join(", "));
    }
    let files = items
        .iter()
        .map(|item| FileError {
            path: match item {
                UnsavedWork::Uncommitted(file)
                | UnsavedWork::Untracked(file)
                | UnsavedWork::Ignored(file) => file.display().to_string(),
                UnsavedWork::Unpushed { branch, .. } => branch.clone(),
            },
            problem: item.kind(),
        })
        .collect();
    workspace_error(&WorkspaceError::UnsavedWork {
        path: path.to_path_buf(),
        items: items.to_vec(),
    })
    .with_message(message)
    .with_files(files)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::error::ExitCode;
    use crate::workspace_cmd::create::{CreateArgs, create};
    use crate::workspace_cmd::test_support::{Fixture, git};

    async fn workspace(fx: &Fixture, name: &str) -> PathBuf {
        let args = CreateArgs {
            repo: None,
            name: Some(name.to_owned()),
            base: None,
            path: None,
            dry_run: false,
        };
        PathBuf::from(create(&args, &fx.context).await.unwrap().path)
    }

    fn args(path: &Path) -> RemoveArgs {
        RemoveArgs {
            path: path.to_path_buf(),
            discard: false,
            dry_run: false,
        }
    }

    async fn registered(fx: &Fixture) -> usize {
        let registry = Registry::open(&fx.context.state_dir).await.unwrap();
        registry.list(None).await.unwrap().len()
    }

    #[tokio::test]
    async fn remove_refuses_uncommitted_changes() {
        let fx = Fixture::new();
        let ws = workspace(&fx, "ws").await;
        fs::write(ws.join("lib.rs"), "fn main() { edited() }\n").unwrap();

        let err = remove(&args(&ws), &fx.context).await.unwrap_err();

        assert_eq!((err.code, err.exit), ("unsaved_work", ExitCode::FAILURE));
        assert_eq!(err.files.len(), 1);
        assert_eq!(
            (err.files[0].path.as_str(), err.files[0].problem),
            ("lib.rs", "uncommitted")
        );
        assert!(err.hint.contains("--discard"));
        assert!(ws.join("lib.rs").is_file());
        assert_eq!(registered(&fx).await, 1);
    }

    #[tokio::test]
    async fn remove_refuses_unpushed_commits() {
        let fx = Fixture::new();
        let ws = workspace(&fx, "ws").await;
        Fixture::commit(&ws, "a.rs");

        let err = remove(&args(&ws), &fx.context).await.unwrap_err();

        assert_eq!(err.code, "unsaved_work");
        assert_eq!(
            (err.files[0].path.as_str(), err.files[0].problem),
            ("ws", "unpushed")
        );
        assert!(
            err.message.contains("1 unpushed commit on ws"),
            "{}",
            err.message
        );
        assert!(ws.is_dir());

        git(&ws, &["push", "--quiet", "origin", "ws"]);
        let removed = remove(&args(&ws), &fx.context).await.unwrap();
        assert_eq!(removed.deleted_branch.as_deref(), Some("ws"));
        assert!(!ws.exists());
    }

    #[tokio::test]
    async fn remove_with_discard_deletes_worktree_and_branch() {
        let fx = Fixture::new();
        let ws = workspace(&fx, "ws").await;
        Fixture::commit(&ws, "a.rs");
        fs::write(ws.join("notes.md"), "draft").unwrap();
        let mut args = args(&ws);
        args.discard = true;

        let removed = remove(&args, &fx.context).await.unwrap();

        assert!(!removed.dry_run);
        assert!(removed.freed_bytes > 0);
        assert_eq!(removed.deleted_branch.as_deref(), Some("ws"));
        assert!(!ws.exists());
        assert!(!openxet_workspace::branch_exists(&fx.main, "ws").unwrap());
        assert!(!git(&fx.main, &["worktree", "list"]).contains("ws"));
        assert_eq!(registered(&fx).await, 0);

        let err = remove(&args, &fx.context).await.unwrap_err();
        assert_eq!(err.code, "not_a_workspace");
    }

    #[tokio::test]
    async fn remove_dry_run_changes_nothing() {
        let fx = Fixture::new();
        let ws = workspace(&fx, "ws").await;
        let mut args = args(&ws);
        args.dry_run = true;

        let removed = remove(&args, &fx.context).await.unwrap();

        assert!(removed.dry_run);
        assert_eq!(removed.deleted_branch.as_deref(), Some("ws"));
        assert!(ws.join("lib.rs").is_file());
        assert!(openxet_workspace::branch_exists(&fx.main, "ws").unwrap());
        assert_eq!(registered(&fx).await, 1);

        fs::write(ws.join("notes.md"), "draft").unwrap();
        let err = remove(&args, &fx.context).await.unwrap_err();
        assert_eq!(
            err.code, "unsaved_work",
            "a dry run refuses as remove would"
        );
    }

    #[tokio::test]
    async fn a_workspace_whose_directory_is_gone_is_unregistered_and_its_branch_kept() {
        let fx = Fixture::new();
        let ws = workspace(&fx, "ws").await;
        Fixture::commit(&ws, "a.rs");
        fs::remove_dir_all(&ws).unwrap();

        let removed = remove(&args(&ws), &fx.context).await.unwrap();

        assert_eq!(removed.deleted_branch, None);
        assert!(openxet_workspace::branch_exists(&fx.main, "ws").unwrap());
        assert_eq!(registered(&fx).await, 0);
    }
}
