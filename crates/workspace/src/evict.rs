//! Eviction: deleting idle workspaces' reconstructible directories to keep
//! the free-space floor, and after the idle TTL. Only what
//! [`reconstructible_dirs`] returns is ever deleted, so durable work stays
//! (spec 2026-10-27 §6.2a).

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{
    Budget, Presets, Registry, Volume, Workspace, WorkspaceError, WorkspaceId, is_idle,
    reconstructible_dirs,
};

/// Cargo's tag at the top of a target directory, which marks it as a cache
/// (<https://bford.info/cachedir/>), with "created by cargo" in its text.
const CACHEDIR_TAG: &str = "CACHEDIR.TAG";

/// The workspaces whose reconstructible directories may be evicted, least
/// recently active first: those no process works in (not in `active`) and
/// idle at `now` ([`is_idle`]).
#[must_use]
pub fn eviction_order<S: BuildHasher>(
    workspaces: &[Workspace],
    now: SystemTime,
    active: &HashSet<WorkspaceId, S>,
) -> Vec<WorkspaceId> {
    let mut idle: Vec<&Workspace> = workspaces
        .iter()
        .filter(|ws| !active.contains(&ws.id) && is_idle(ws.last_active, now))
        .collect();
    idle.sort_by_key(|ws| (ws.last_active, ws.id));
    idle.into_iter().map(|ws| ws.id).collect()
}

/// Keeps the free-space floor: on each volume under its floor, evicts idle
/// workspaces' reconstructible directories in [`eviction_order`] until free
/// space is 5% over the floor, so the next build's first files don't put it
/// straight back under. Cargo's `incremental/` directories go first, in
/// every idle workspace, before any whole directory: measurement S16 found
/// them the least shared between workspaces and the cheapest to lose (Cargo
/// keeps every build's result and recompiles only what changes next).
///
/// `sample_activity` runs only when a volume is under its floor; when it
/// can't tell which workspaces are in use, nothing is evicted.
/// `volume_of` measures the volume a path is on.
///
/// Returns the directories evicted.
///
/// # Errors
///
/// [`WorkspaceError::Database`] if the registry can't be read or written,
/// [`WorkspaceError::Io`] if a volume can't be measured or a directory
/// can't be deleted.
pub async fn free_to_floor(
    reg: &Registry,
    budget: &Budget,
    now: SystemTime,
    sample_activity: impl FnOnce(&[Workspace]) -> Option<HashSet<WorkspaceId>>,
    volume_of: impl Fn(&Path) -> Result<Volume, WorkspaceError>,
) -> Result<Vec<PathBuf>, WorkspaceError> {
    let workspaces = reg.list(None).await?;
    let mut short = HashSet::new();
    let mut devices = HashMap::new();
    for ws in &workspaces {
        let Ok(meta) = std::fs::symlink_metadata(&ws.path) else {
            continue;
        };
        devices.insert(ws.id, meta.dev());
        let volume = volume_of(&ws.path)?;
        if volume.free_bytes < budget.floor_for(volume) {
            short.insert(meta.dev());
        }
    }
    if short.is_empty() {
        return Ok(Vec::new());
    }
    let Some(active) = sample_activity(&workspaces) else {
        tracing::warn!(
            "free space is under the floor, but which workspaces are in use is unknown, so \
             nothing was evicted"
        );
        return Ok(Vec::new());
    };
    let order: Vec<&Workspace> = eviction_order(&workspaces, now, &active)
        .into_iter()
        .filter_map(|id| workspaces.iter().find(|ws| ws.id == id))
        .collect();
    let needs_space = |ws: &Workspace| -> Result<bool, WorkspaceError> {
        if !devices.get(&ws.id).is_some_and(|dev| short.contains(dev)) {
            return Ok(false);
        }
        let volume = volume_of(&ws.path)?;
        let floor = budget.floor_for(volume);
        Ok(volume.free_bytes < floor.saturating_add(floor / 20))
    };

    let mut evicted = Vec::new();
    for ws in &order {
        for dir in cargo_incremental_dirs(&ws.path) {
            if !needs_space(ws)? {
                break;
            }
            evict_dir(reg, &dir).await?;
            evicted.push(dir);
        }
    }
    for ws in &order {
        for dir in reconstructible_dirs(&ws.path, &presets(&ws.path)) {
            if !needs_space(ws)? {
                break;
            }
            evict_dir(reg, &dir).await?;
            evicted.push(dir);
        }
    }
    Ok(evicted)
}

/// Evicts every reconstructible directory of the workspaces no process works
/// in (not in `active`) and idle for at least [`Budget::idle_ttl`] at `now`.
/// Returns the directories evicted.
///
/// # Errors
///
/// [`WorkspaceError::Database`] if the registry can't be read or written,
/// [`WorkspaceError::Io`] if a directory can't be deleted.
pub async fn evict_expired<S: BuildHasher>(
    reg: &Registry,
    budget: &Budget,
    now: SystemTime,
    active: &HashSet<WorkspaceId, S>,
) -> Result<Vec<PathBuf>, WorkspaceError> {
    let mut evicted = Vec::new();
    for ws in reg.list(None).await? {
        let expired = now
            .duration_since(ws.last_active)
            .is_ok_and(|idle| idle >= budget.idle_ttl);
        if !expired || active.contains(&ws.id) {
            continue;
        }
        for dir in reconstructible_dirs(&ws.path, &presets(&ws.path)) {
            evict_dir(reg, &dir).await?;
            evicted.push(dir);
        }
    }
    Ok(evicted)
}

/// The workspace's presets. An invalid project config fails the commands
/// that read it; eviction still takes the default presets, which are
/// reconstructible whatever the config says.
fn presets(root: &Path) -> Presets {
    Presets::load(root).unwrap_or_default()
}

/// The `incremental/` directories of the Cargo target directories among
/// `root`'s reconstructible directories: `<target>/<profile>/incremental`
/// and, for a `--target` build, `<target>/<triple>/<profile>/incremental`.
/// A directory without Cargo's [`CACHEDIR_TAG`] is not Cargo's and is only
/// ever evicted whole, since other tools don't rebuild a missing part.
fn cargo_incremental_dirs(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for target in reconstructible_dirs(root, &presets(root)) {
        let tagged = std::fs::read_to_string(target.join(CACHEDIR_TAG))
            .is_ok_and(|tag| tag.contains("created by cargo"));
        if !tagged {
            continue;
        }
        for profile in real_subdirs(&target) {
            for sub in real_subdirs(&profile) {
                if sub.file_name().is_some_and(|name| name == "incremental") {
                    found.push(sub);
                } else if is_real_dir(&sub.join("incremental")) {
                    found.push(sub.join("incremental"));
                }
            }
        }
    }
    found
}

/// The directories directly in `dir`, without following symbolic links, so
/// nothing found leads outside it.
fn real_subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Deletes `dir` and drops the dedup index's entries under it, so no
/// canonical copy points at a deleted file. `remove_dir_all` doesn't
/// follow symbolic links inside it.
async fn evict_dir(reg: &Registry, dir: &Path) -> Result<(), WorkspaceError> {
    let owned = dir.to_path_buf();
    let removed = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&owned))
        .await
        .unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()));
    // Forgotten even when the removal failed partway: some files are gone.
    reg.forget_canonical(dir).await?;
    match removed {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(WorkspaceError::io(dir)(err)),
    }
    tracing::info!(dir = %dir.display(), "evicted reconstructible directory");
    Ok(())
}
