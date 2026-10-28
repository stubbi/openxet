use std::path::Path;
use std::process::{Command, Output};

use crate::WorkspaceError;

/// Runs `git -C <dir> <args>` and returns its output whatever its exit
/// status, so callers can tell Git's answers (`check-ignore` exits 1 for
/// "not ignored") from failures.
///
/// The variables that point Git at another repository are cleared: when
/// OpenXet runs inside a Git hook they are set for the hook's repository,
/// not for `dir`.
pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<Output, WorkspaceError> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|err| WorkspaceError::Git {
            command: args.join(" "),
            message: format!("could not run git ({err}); install Git and put it on PATH"),
        })
}

/// The `Git` error for a command that exited unsuccessfully.
pub(crate) fn failed(args: &[&str], output: &Output) -> WorkspaceError {
    WorkspaceError::Git {
        command: args.join(" "),
        message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}
