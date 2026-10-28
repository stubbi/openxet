//! `openxet workspace`: Git worktrees for agents working in parallel, whose
//! build outputs start as copy-on-write clones of a sibling's (spec
//! 2026-10-27), registered per user so they can be listed and removed
//! without losing work.

mod create;
mod names;
mod remove;
mod report;

use std::path::{Path, PathBuf};

use openxet_workspace::WorkspaceError;

use crate::error::{CliError, ExitCode};
use crate::output::Output;

#[derive(Debug, clap::Subcommand)]
pub enum WorkspaceCommand {
    /// Create a workspace: a worktree on a new branch, with build outputs
    /// cloned from the nearest sibling
    Create(create::CreateArgs),
    /// List the registered workspaces
    List(report::ListArgs),
    /// Report free space, the free-space floor and clone support here
    Status,
    /// Remove a workspace and its branch, refusing if that would lose work
    Remove(remove::RemoveArgs),
}

/// Where the command runs: the registry's directory and the directory
/// relative paths start from. Passed in, not read from the process, so
/// tests can run side by side.
pub(crate) struct Context {
    pub state_dir: PathBuf,
    pub cwd: PathBuf,
}

pub fn run(command: &WorkspaceCommand, output: Output) -> Result<(), CliError> {
    let context = Context {
        state_dir: openxet_workspace::state_dir().map_err(|err| workspace_error(&err))?,
        cwd: std::env::current_dir().map_err(|err| {
            CliError::new(
                "io",
                format!("could not read the current directory: {err}"),
                "Run the command from a directory that exists and that you may read.",
            )
        })?,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            CliError::new(
                "runtime",
                format!("could not start the async runtime: {err}"),
                "Check the process's thread and file-descriptor limits.",
            )
        })?;
    runtime.block_on(async {
        match command {
            WorkspaceCommand::Create(args) => {
                let created = create::create(args, &context).await?;
                emit(output, &created)
            }
            WorkspaceCommand::List(args) => emit(output, &report::list(args, &context).await?),
            WorkspaceCommand::Status => emit(output, &report::status(&context).await?),
            WorkspaceCommand::Remove(args) => emit(output, &remove::remove(args, &context).await?),
        }
    })
}

fn emit<T: serde::Serialize + crate::output::HumanDisplay>(
    output: Output,
    value: &T,
) -> Result<(), CliError> {
    output.emit(value).map_err(|err| CliError::output(&err))
}

/// `path` relative to the command's directory.
fn absolute(context: &Context, path: &Path) -> PathBuf {
    context.cwd.join(path)
}

/// The CLI's form of a workspace error: its stable code, its message and
/// the next action to take.
pub(crate) fn workspace_error(err: &WorkspaceError) -> CliError {
    let (hint, exit) = match err {
        WorkspaceError::NotARepository { .. } => (
            "Run it inside a worktree of the repository, or pass `--repo DIR`; `git init` makes \
             a new repository.",
            ExitCode::FAILURE,
        ),
        WorkspaceError::NotAWorkspace(_) => (
            "Pass the path `openxet workspace create` printed; `openxet workspace list` shows \
             them all.",
            ExitCode::FAILURE,
        ),
        WorkspaceError::UnknownRevision(_) => (
            "Pass `--base` a branch, tag or commit of this repository, such as `main` or \
             `HEAD`.",
            ExitCode::USAGE,
        ),
        WorkspaceError::Database(_) | WorkspaceError::Migrate(_) | WorkspaceError::Corrupt(_) => (
            "The registry is in OpenXet's state directory; set OPENXET_WORKSPACE_STATE to use \
             another.",
            ExitCode::FAILURE,
        ),
        WorkspaceError::Git { .. } => (
            "Fix what Git reports, then run the command again.",
            ExitCode::FAILURE,
        ),
        WorkspaceError::Config { .. } => (
            "Fix `.openxet/workspace.toml` in the repository, then run the command again.",
            ExitCode::FAILURE,
        ),
        WorkspaceError::UnsavedWork { .. } => (remove::UNSAVED_HINT, ExitCode::FAILURE),
        WorkspaceError::DiskBudget { .. } => (
            "Retry in a minute, or remove idle workspaces with `openxet workspace list` and \
             `remove`.",
            ExitCode::FAILURE,
        ),
        WorkspaceError::Io { .. } => (
            "Check the path's permissions and that its disk is not full, then run the command \
             again.",
            ExitCode::FAILURE,
        ),
    };
    CliError::new(err.code(), err.to_string(), hint).with_exit(exit)
}

/// `bytes` for people: in the largest binary unit that keeps it at least
/// 1, with one decimal.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 * 1024 && unit + 1 < UNITS.len() {
        value /= 1024;
        unit += 1;
    }
    // Tenths of the unit, in integers: `value` is under 1024 * 1024.
    let tenths = value * 10 / 1024;
    format!("{}.{} {}", tenths / 10, tenths % 10, UNITS[unit])
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_read_in_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(5 << 30), "5.0 GiB");
        assert_eq!(human_bytes(u64::MAX), "16383.9 PiB");
    }
}
