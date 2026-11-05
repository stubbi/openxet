//! `openxet workspace list` and `status`.

use std::fmt::Write as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use openxet_workspace::{
    Presets, ProjectId, Registry, WorkspaceError, allocated_bytes, daemon_pid, daemon_running,
    is_idle, reconstructible_dirs, reflink_supported, volume_reflink_supported,
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
    /// Whether it has seen no activity for 10 minutes, or for `idle_ttl`
    /// when that is shorter: what makes it a candidate for floor eviction.
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
    let idle_after = context.budget()?.idle_after();
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
            idle: is_idle(workspace.last_active, now, idle_after),
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
    /// Where the current volume was measured: the current repository's Git
    /// directory, or outside a repository the registry's. The fields up to
    /// `reflink_supported` are this volume's; `volumes` has every volume.
    volume_path: String,
    volume_free_bytes: u64,
    /// The free space kept free: `floor` in the user's workspace config,
    /// else the larger of 20 GiB and a tenth of the volume.
    floor_bytes: u64,
    /// Whether files on the volume can be cloned. Without clones, workspaces
    /// are not seeded.
    reflink_supported: bool,
    daemon_running: bool,
    /// The daemon's process id, when it runs.
    daemon_pid: Option<u32>,
    /// How many workspaces are registered, in every repository.
    workspaces: usize,
    /// Bytes of files the daemon has replaced by clones of an identical
    /// file, in total; files rewritten or evicted since are not subtracted.
    deduplicated_bytes: u64,
    /// The current volume, then each other volume that holds a registered
    /// workspace: the daemon keeps the floor on each of them.
    volumes: Vec<VolumeStatus>,
}

/// One volume's free space and floor.
#[derive(Debug, Serialize)]
struct VolumeStatus {
    /// Where it was measured: `volume_path` for the current volume, else
    /// its first registered workspace.
    path: String,
    free_bytes: u64,
    floor_bytes: u64,
    reflink_supported: bool,
    /// How many registered workspaces are on it.
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
        let daemon = match (self.daemon_running, self.daemon_pid) {
            (true, Some(pid)) => format!(
                "Daemon: running (pid {pid}); `openxet workspace daemon restart` applies changed \
                 limits."
            ),
            (true, None) => "Daemon: running.".to_owned(),
            (false, _) => "Daemon: not running; `openxet workspace create` starts it.".to_owned(),
        };
        let mut text = format!(
            "Volume of {}: {} free, floor {}.\n{clones}\n{daemon}\nWorkspaces: {} registered.\n\
             Deduplicated: {} replaced by clones.",
            self.volume_path,
            human_bytes(self.volume_free_bytes),
            human_bytes(self.floor_bytes),
            self.workspaces,
            human_bytes(self.deduplicated_bytes)
        );
        for other in self.volumes.iter().skip(1) {
            let _ = write!(
                text,
                "\nVolume of {}: {} free, floor {}, {} workspaces.",
                other.path,
                human_bytes(other.free_bytes),
                human_bytes(other.floor_bytes),
                other.workspaces
            );
        }
        text
    }
}

pub(super) async fn status(context: &Context) -> Result<Status, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let budget = context.budget()?;
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let registered = registry.list(None).await.map_err(error)?;
    // The Git directory is on the volume new workspaces go to by default,
    // and writable, which the clone probe needs.
    let dir = ProjectId::discover(&context.cwd)
        .map_or_else(|_| context.state_dir.clone(), |p| PathBuf::from(p.as_str()));
    let here = std::fs::metadata(&dir)
        .map_err(|source| {
            error(WorkspaceError::Io {
                path: dir.clone(),
                source,
            })
        })?
        .dev();
    // A workspace whose directory is gone is on no volume.
    let on_volumes = registered.iter().filter_map(|ws| {
        std::fs::metadata(&ws.path)
            .ok()
            .map(|meta| (meta.dev(), ws.path.clone()))
    });
    let mut volumes = Vec::new();
    for (n, (path, workspaces)) in by_volume((here, dir), on_volumes).into_iter().enumerate() {
        let measured = (context.volume)(&path).map_err(error)?;
        volumes.push(VolumeStatus {
            path: path.display().to_string(),
            free_bytes: measured.free_bytes,
            floor_bytes: budget.floor_for(measured),
            // Every other volume's path is a workspace, which a probe in it
            // would touch.
            reflink_supported: if n == 0 {
                reflink_supported(&path)
            } else {
                volume_reflink_supported(&path)
            },
            workspaces,
        });
    }
    let daemon_pid = daemon_pid(&context.state_dir);
    let current = &volumes[0];
    Ok(Status {
        schema: "openxet.workspace.status.v1",
        volume_path: current.path.clone(),
        volume_free_bytes: current.free_bytes,
        floor_bytes: current.floor_bytes,
        reflink_supported: current.reflink_supported,
        daemon_running: daemon_pid.is_some() || daemon_running(&context.state_dir),
        daemon_pid,
        workspaces: registered.len(),
        deduplicated_bytes: registry.deduplicated_bytes().await.map_err(error)?,
        volumes,
    })
}

/// The volumes `here` and the `workspaces` are on, each given as
/// `(device, path)`: `here` first, then each other device in the order its
/// first workspace comes, each with a path on it and how many workspaces
/// it holds.
fn by_volume(
    here: (u64, PathBuf),
    workspaces: impl IntoIterator<Item = (u64, PathBuf)>,
) -> Vec<(PathBuf, usize)> {
    let mut volumes = vec![(here.0, here.1, 0)];
    for (dev, path) in workspaces {
        match volumes.iter_mut().find(|(seen, _, _)| *seen == dev) {
            Some((_, _, count)) => *count += 1,
            None => volumes.push((dev, path, 1)),
        }
    }
    volumes
        .into_iter()
        .map(|(_, path, count)| (path, count))
        .collect()
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
        assert_eq!(status.daemon_pid, None);
        assert_eq!(status.workspaces, 0);
        assert_eq!(status.deduplicated_bytes, 0);
        assert_eq!(status.volumes.len(), 1);
        assert_eq!(status.volumes[0].path, status.volume_path);
        assert_eq!(status.volumes[0].workspaces, 0);
    }

    #[tokio::test]
    async fn status_counts_workspaces_on_each_volume() {
        let fx = Fixture::new();
        let mut args = CreateArgs {
            repo: None,
            name: Some("elsewhere".to_owned()),
            base: None,
            path: Some(fx.main.parent().unwrap().join("elsewhere")),
            dry_run: false,
        };
        create(&args, &fx.context).await.unwrap();
        args.name = Some("default".to_owned());
        args.path = None;
        create(&args, &fx.context).await.unwrap();

        let status = status(&fx.context).await.unwrap();

        // The test's directories are all on one volume.
        assert_eq!(status.volumes.len(), 1);
        assert_eq!(status.volumes[0].workspaces, 2);
        assert_eq!(status.workspaces, 2);
    }

    #[test]
    fn volumes_are_grouped_by_device_with_the_current_one_first() {
        let here = (1, PathBuf::from("/repo/.git"));
        let workspaces = [
            (2, PathBuf::from("/mnt/fast/a")),
            (1, PathBuf::from("/repo-worktrees/b")),
            (2, PathBuf::from("/mnt/fast/c")),
            (3, PathBuf::from("/mnt/big/d")),
        ];

        let volumes = by_volume(here, workspaces);

        assert_eq!(
            volumes,
            vec![
                (PathBuf::from("/repo/.git"), 1),
                (PathBuf::from("/mnt/fast/a"), 2),
                (PathBuf::from("/mnt/big/d"), 1),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn status_reports_a_running_daemon() {
        let fx = Fixture::new();
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let state = fx.context.state_dir.clone();
        let daemon = tokio::spawn(async move {
            openxet_workspace::run_daemon(&state, openxet_workspace::Budget::default(), async {
                let _ = stopped.await;
            })
            .await
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !status(&fx.context).await.unwrap().daemon_running {
            assert!(std::time::Instant::now() < deadline, "the daemon never ran");
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        let running = status(&fx.context).await.unwrap();
        assert_eq!(running.daemon_pid, Some(std::process::id()));
        assert!(running.human().contains("daemon restart"));

        stop.send(()).unwrap();
        daemon.await.unwrap().unwrap();

        assert!(!status(&fx.context).await.unwrap().daemon_running);
    }
}
