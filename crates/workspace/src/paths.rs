use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::{WorkspaceError, git};

/// Overrides [`state_dir`], for tests and for humans who keep state
/// elsewhere.
pub const STATE_DIR_ENV: &str = "OPENXET_WORKSPACE_STATE";

/// The per-user directory that holds the workspace registry:
/// `$OPENXET_WORKSPACE_STATE` if set, else
/// `~/Library/Application Support/openxet/workspaces` on macOS and
/// `$XDG_STATE_HOME/openxet/workspaces` (by default
/// `~/.local/state/openxet/workspaces`) elsewhere. A relative
/// `XDG_STATE_HOME` is ignored, as the XDG Base Directory spec requires
/// (<https://specifications.freedesktop.org/basedir-spec/latest/>).
///
/// # Errors
///
/// [`WorkspaceError::Io`] if the override is unset and there is no home
/// directory to put the state under.
pub fn state_dir() -> Result<PathBuf, WorkspaceError> {
    state_dir_from(
        std::env::var_os(STATE_DIR_ENV),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::home_dir(),
    )
}

fn state_dir_from(
    over: Option<OsString>,
    xdg_state_home: Option<OsString>,
    home: Option<PathBuf>,
) -> Result<PathBuf, WorkspaceError> {
    if let Some(dir) = over.filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let xdg = xdg_state_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute());
    let base = match (cfg!(target_os = "macos"), xdg, home) {
        (true, _, Some(home)) => home.join("Library/Application Support"),
        (false, Some(xdg), _) => xdg,
        (false, None, Some(home)) => home.join(".local/state"),
        (_, _, None) => {
            return Err(WorkspaceError::Io {
                path: PathBuf::from("~"),
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "no home directory; set {STATE_DIR_ENV} to a directory for OpenXet's workspace state"
                    ),
                ),
            });
        }
    };
    Ok(base.join("openxet/workspaces"))
}

/// A project: one Git repository, named by the canonical path of its common
/// Git directory, so every worktree of a repository has the same project.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectId(String);

impl ProjectId {
    /// The project of the repository `dir` is in, from
    /// `git rev-parse --git-common-dir`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::NotARepository`] if `dir` is not inside a Git
    /// repository, [`WorkspaceError::Git`] if Git fails otherwise, and
    /// [`WorkspaceError::Io`] if the directory Git names can't be resolved.
    pub fn discover(dir: &Path) -> Result<Self, WorkspaceError> {
        let args = ["rev-parse", "--path-format=absolute", "--git-common-dir"];
        let output = git::run(dir, &args)?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Git's wording since 1.x; also what it says for a missing
            // directory under `-C`.
            if stderr.contains("not a git repository") || stderr.contains("cannot change to") {
                return Err(WorkspaceError::NotARepository {
                    path: dir.to_path_buf(),
                });
            }
            return Err(git::failed(&args, &output));
        }
        let common = String::from_utf8(output.stdout).map_err(|_| WorkspaceError::Git {
            command: args.join(" "),
            message: "the repository's path is not UTF-8; move it to a UTF-8 path".to_owned(),
        })?;
        let common = Path::new(common.trim_end_matches('\n'));
        let canonical = common.canonicalize().map_err(WorkspaceError::io(common))?;
        let name = canonical
            .into_os_string()
            .into_string()
            .map_err(|_| WorkspaceError::Git {
                command: args.join(" "),
                message: "the repository's path is not UTF-8; move it to a UTF-8 path".to_owned(),
            })?;
        Ok(Self(name))
    }

    /// The canonical path of the repository's common Git directory.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_stored(name: String) -> Self {
        Self(name)
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn override_wins_and_empty_override_is_ignored() {
        let home = Some(PathBuf::from("/home/a"));
        assert_eq!(
            state_dir_from(Some("/s".into()), None, home.clone()).unwrap(),
            PathBuf::from("/s")
        );
        let default = state_dir_from(Some(OsString::new()), None, home).unwrap();
        assert!(default.ends_with("openxet/workspaces"));
    }

    #[test]
    fn platform_default_under_home() {
        let home = Some(PathBuf::from("/home/a"));
        let got = state_dir_from(None, Some("relative".into()), home).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(
                got,
                PathBuf::from("/home/a/Library/Application Support/openxet/workspaces")
            );
        } else {
            assert_eq!(
                got,
                PathBuf::from("/home/a/.local/state/openxet/workspaces")
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn absolute_xdg_state_home_is_used() {
        let got = state_dir_from(None, Some("/xdg".into()), Some("/home/a".into())).unwrap();
        assert_eq!(got, PathBuf::from("/xdg/openxet/workspaces"));
    }

    #[test]
    fn no_home_is_an_actionable_error() {
        let err = state_dir_from(None, None, None).unwrap_err();
        assert_eq!(err.code(), "io");
        assert!(err.to_string().contains(STATE_DIR_ENV));
    }
}
