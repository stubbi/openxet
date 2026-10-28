//! Which workspaces a process is working in right now: a shell, an agent or
//! a build whose current directory is inside one.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::{Workspace, WorkspaceId};

/// The workspaces some process has its current directory in, or `None`
/// when the processes' directories can't be listed, so callers treat every
/// workspace as possibly in use. Only the user's own processes are seen,
/// which are the ones that work in the user's workspaces.
#[must_use]
pub fn active_workspaces(workspaces: &[Workspace]) -> Option<HashSet<WorkspaceId>> {
    Some(active_among(workspaces, &process_cwds()?))
}

fn active_among(workspaces: &[Workspace], cwds: &[PathBuf]) -> HashSet<WorkspaceId> {
    workspaces
        .iter()
        .filter(|ws| cwds.iter().any(|cwd| cwd.starts_with(&ws.path)))
        .map(|ws| ws.id)
        .collect()
}

/// Every visible process's current directory, from `lsof`'s field output
/// (`-F n`: one `n<path>` line per file, here only `cwd`;
/// <https://www.man7.org/linux/man-pages/man8/lsof.8.html>, OUTPUT FOR
/// OTHER PROGRAMS). macOS has no `/proc`, and `lsof` ships with it.
#[cfg(target_os = "macos")]
fn process_cwds() -> Option<Vec<PathBuf>> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let output = std::process::Command::new("lsof")
        .args(["-w", "-d", "cwd", "-F", "n"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    // `lsof` exits 1 when one process can't be inspected, with the others
    // listed; no output at all means it failed.
    if output.stdout.is_empty() {
        return None;
    }
    Some(
        output
            .stdout
            .split(|&b| b == b'\n')
            .filter_map(|line| line.strip_prefix(b"n"))
            .map(|path| PathBuf::from(OsStr::from_bytes(path)))
            .collect(),
    )
}

/// Every visible process's current directory, from the `/proc/<pid>/cwd`
/// links (proc_pid_cwd(5)). Other users' processes can't be read and are
/// left out.
#[cfg(target_os = "linux")]
fn process_cwds() -> Option<Vec<PathBuf>> {
    let entries = std::fs::read_dir("/proc").ok()?;
    Some(
        entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
            })
            .filter_map(|entry| std::fs::read_link(entry.path().join("cwd")).ok())
            .collect(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_cwds() -> Option<Vec<PathBuf>> {
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::path::Path;
    use std::time::SystemTime;

    use super::*;
    use crate::ProjectId;

    fn workspace(path: &Path) -> Workspace {
        Workspace {
            id: WorkspaceId::generate(),
            project: ProjectId::from_stored("/repo/.git".to_owned()),
            path: path.to_path_buf(),
            branch: None,
            created_at: SystemTime::now(),
            last_active: SystemTime::now(),
            adopted: false,
        }
    }

    #[test]
    fn a_cwd_inside_a_workspace_makes_it_active() {
        let one = workspace(Path::new("/ws/one"));
        let sibling = workspace(Path::new("/ws/one-more"));
        let cwds = [PathBuf::from("/ws/one/src"), PathBuf::from("/elsewhere")];

        let active = active_among(&[one.clone(), sibling], &cwds);

        assert_eq!(active, HashSet::from([one.id]));
    }

    #[test]
    fn this_process_is_seen_in_its_own_directory() {
        let here = std::env::current_dir().unwrap().canonicalize().unwrap();
        let ws = workspace(&here);
        let active = active_workspaces(std::slice::from_ref(&ws)).unwrap();
        assert!(active.contains(&ws.id));
    }
}
