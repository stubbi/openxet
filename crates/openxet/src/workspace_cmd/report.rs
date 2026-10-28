//! `openxet workspace list` and `status`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use openxet_workspace::{
    Presets, ProjectId, Registry, WorkspaceError, allocated_bytes, default_floor, is_idle,
    reconstructible_dirs, reflink_supported, volume,
};
use serde::Serialize;

use super::{Context, absolute, human_bytes, workspace_error};
use crate::error::CliError;
use crate::output::HumanDisplay;

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    /// Only the workspaces of the repository this directory is in
    /// [default: every repository's]
    #[arg(long, value_name = "DIR")]
    repo: Option<PathBuf>,
}

/// The `openxet.workspace.list.v1` result.
#[derive(Debug, Serialize)]
pub struct Listed {
    schema: &'static str,
    workspaces: Vec<Entry>,
}

#[derive(Debug, Serialize)]
struct Entry {
    path: String,
    name: String,
    branch: Option<String>,
    /// When it last saw activity, in seconds since the Unix epoch.
    last_active: u64,
    /// What its reconstructible directories take on disk, clones counted
    /// in full.
    reconstructible_bytes: u64,
    /// Whether it has seen no activity for 10 minutes.
    idle: bool,
}

impl HumanDisplay for Listed {
    fn human(&self) -> String {
        if self.workspaces.is_empty() {
            return "No workspaces. `openxet workspace create` makes one.".to_owned();
        }
        let mut text = String::new();
        for entry in &self.workspaces {
            let state = if entry.idle { "idle" } else { "active" };
            let branch = entry.branch.as_deref().unwrap_or("(detached)");
            let _ = writeln!(
                text,
                "{}  {branch}  {state}  {} reconstructible  {}",
                entry.name,
                human_bytes(entry.reconstructible_bytes),
                entry.path
            );
        }
        text.pop();
        text
    }
}

pub(super) async fn list(args: &ListArgs, context: &Context) -> Result<Listed, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let project = match &args.repo {
        Some(dir) => Some(ProjectId::discover(&absolute(context, dir)).map_err(error)?),
        None => None,
    };
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let now = SystemTime::now();
    let workspaces = registry
        .list(project.as_ref())
        .await
        .map_err(error)?
        .into_iter()
        .map(|workspace| Entry {
            name: workspace
                .path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            reconstructible_bytes: reconstructible_bytes(&workspace.path),
            path: workspace.path.display().to_string(),
            branch: workspace.branch,
            last_active: workspace
                .last_active
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            idle: is_idle(workspace.last_active, now),
        })
        .collect();
    Ok(Listed {
        schema: "openxet.workspace.list.v1",
        workspaces,
    })
}

fn reconstructible_bytes(root: &Path) -> u64 {
    // An invalid project config fails the commands that act on it; a
    // listing shows what the defaults find rather than nothing.
    let presets = Presets::load(root).unwrap_or_default();
    reconstructible_dirs(root, &presets)
        .iter()
        .map(|dir| allocated_bytes(dir))
        .sum()
}

/// The `openxet.workspace.status.v1` result.
#[derive(Debug, Serialize)]
pub struct Status {
    schema: &'static str,
    /// Where the volume was measured: the current repository's Git
    /// directory, or outside a repository the registry's.
    volume_path: String,
    volume_free_bytes: u64,
    /// The free space kept free: the larger of 20 GiB and a tenth of the
    /// volume.
    floor_bytes: u64,
    /// Whether files on the volume can be cloned. Without clones, workspaces
    /// are not seeded.
    reflink_supported: bool,
    daemon_running: bool,
    /// How many workspaces are registered, in every repository.
    workspaces: usize,
}

impl HumanDisplay for Status {
    fn human(&self) -> String {
        let clones = if self.reflink_supported {
            "Copy-on-write clones: supported."
        } else {
            "Copy-on-write clones: not supported on this volume, so new workspaces are not \
             seeded; OpenXet never copies build outputs in full."
        };
        let daemon = if self.daemon_running {
            "Daemon: running."
        } else {
            "Daemon: not running."
        };
        format!(
            "Volume of {}: {} free, floor {}.\n{clones}\n{daemon}\nWorkspaces: {} registered.",
            self.volume_path,
            human_bytes(self.volume_free_bytes),
            human_bytes(self.floor_bytes),
            self.workspaces
        )
    }
}

pub(super) async fn status(context: &Context) -> Result<Status, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let workspaces = registry.list(None).await.map_err(error)?.len();
    // The Git directory is on the volume new workspaces go to by default,
    // and writable, which the clone probe needs.
    let dir = ProjectId::discover(&context.cwd)
        .map_or_else(|_| context.state_dir.clone(), |p| PathBuf::from(p.as_str()));
    let here = volume(&dir).map_err(error)?;
    Ok(Status {
        schema: "openxet.workspace.status.v1",
        volume_path: dir.display().to_string(),
        volume_free_bytes: here.free_bytes,
        floor_bytes: default_floor(here.total_bytes),
        reflink_supported: reflink_supported(&dir),
        // The daemon (plan 2026-10-27, Task 6) does not exist yet.
        daemon_running: false,
        workspaces,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::workspace_cmd::create::{CreateArgs, create};
    use crate::workspace_cmd::test_support::Fixture;

    #[tokio::test]
    async fn list_shows_each_workspace_with_its_build_outputs() {
        let fx = Fixture::new();
        let args = CreateArgs {
            repo: None,
            name: Some("one".to_owned()),
            base: None,
            path: None,
            dry_run: false,
        };
        let path = PathBuf::from(create(&args, &fx.context).await.unwrap().path);
        fs::create_dir_all(path.join("target")).unwrap();
        fs::write(path.join("target/out"), vec![0u8; 64 << 10]).unwrap();

        let listed = list(&ListArgs { repo: None }, &fx.context).await.unwrap();

        assert_eq!(listed.workspaces.len(), 1);
        let entry = &listed.workspaces[0];
        assert_eq!(
            (entry.name.as_str(), entry.branch.as_deref()),
            ("one", Some("one"))
        );
        assert!(entry.reconstructible_bytes >= 64 << 10);
        assert!(!entry.idle);
        assert!(listed.human().contains("one  one  active"));
    }

    #[tokio::test]
    async fn status_reports_the_volume_and_the_floor() {
        let fx = Fixture::new();
        let status = status(&fx.context).await.unwrap();
        assert_eq!(PathBuf::from(&status.volume_path), fx.main.join(".git"));
        assert!(status.floor_bytes >= 20 << 30);
        assert!(!status.daemon_running);
        assert_eq!(status.workspaces, 0);
    }
}
