use std::path::PathBuf;

use crate::UnsavedWork;

/// What can go wrong with a workspace. Each variant maps to a stable
/// [`code`](WorkspaceError::code) that `openxet workspace --json` reports in
/// `openxet.error.v1`, so agents can branch on it.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The directory is not inside a Git repository.
    #[error(
        "{} is not inside a Git repository; run the command from a repository or pass --repo",
        .path.display()
    )]
    NotARepository {
        /// The directory that was asked about.
        path: PathBuf,
    },
    /// No registered workspace matches.
    #[error(
        "{0} is not a registered workspace; `openxet workspace list` shows the registered ones"
    )]
    NotAWorkspace(String),
    /// The registry database could not be opened, read or written.
    #[error(
        "workspace registry error ({0}); check that the state directory is writable and the disk \
         is not full, then retry"
    )]
    Database(#[source] sqlx::Error),
    /// The registry schema could not be brought up to date.
    #[error(
        "could not migrate the workspace registry; it may be from a newer OpenXet, so run that \
         version"
    )]
    Migrate(#[source] sqlx::migrate::MigrateError),
    /// A stored registry row doesn't decode.
    #[error(
        "the workspace registry holds a corrupt entry ({0}); please report it at \
         https://github.com/stubbi/openxet/issues"
    )]
    Corrupt(String),
    /// A Git command failed for another reason than a missing repository.
    #[error("`git {command}` failed: {message}")]
    Git {
        /// The Git subcommand and its arguments.
        command: String,
        /// Git's own error, or why it could not run.
        message: String,
    },
    /// The project's `.openxet/workspace.toml` is invalid.
    #[error("{} is invalid: {message}", .path.display())]
    Config {
        /// The config file.
        path: PathBuf,
        /// What is wrong and how to fix it.
        message: String,
    },
    /// Removing the workspace would lose work that exists nowhere else.
    #[error(
        "{} holds work that exists nowhere else: {}; commit and push it, or pass --discard to \
         delete it",
        .path.display(),
        .items.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")
    )]
    UnsavedWork {
        /// The workspace.
        path: PathBuf,
        /// What would be lost, as [`unsaved_work`](crate::unsaved_work)
        /// found it.
        items: Vec<UnsavedWork>,
    },
    /// A revision names no commit.
    #[error("{0:?} names no commit in this repository")]
    UnknownRevision(String),
    /// Free space is under the floor, even after evicting idle workspaces.
    #[error(
        "{free_bytes} bytes free, under the floor of {floor_bytes} bytes; retry in a minute, or \
         remove idle workspaces with `openxet workspace list` and `remove`"
    )]
    DiskBudget {
        /// Free bytes on the volume.
        free_bytes: u64,
        /// The free-space floor.
        floor_bytes: u64,
    },
    /// Another daemon already serves this state directory.
    #[error(
        "another OpenXet workspace daemon holds {}, so it is already running",
        .lock.display()
    )]
    DaemonRunning {
        /// The daemon's lock file.
        lock: PathBuf,
    },
    /// A file operation failed.
    #[error("{}: {source}", .path.display())]
    Io {
        /// The path the operation was on.
        path: PathBuf,
        /// What failed.
        #[source]
        source: std::io::Error,
    },
}

impl WorkspaceError {
    /// The stable, machine-readable code for this error.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotARepository { .. } => "not_a_repository",
            Self::NotAWorkspace(_) => "not_a_workspace",
            Self::Database(_) | Self::Migrate(_) | Self::Corrupt(_) => "registry",
            Self::Git { .. } => "git",
            Self::Config { .. } => "config",
            Self::UnsavedWork { .. } => "unsaved_work",
            Self::UnknownRevision(_) => "unknown_revision",
            Self::DiskBudget { .. } => "disk_budget",
            Self::DaemonRunning { .. } => "daemon_running",
            Self::Io { .. } => "io",
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }
}

impl From<sqlx::Error> for WorkspaceError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}
