//! `openxet workspace daemon`, hidden: the per-user daemon `create` starts
//! in the background, and starting it.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};

use openxet_workspace::{STATE_DIR_ENV, daemon_running, run_daemon};
use tokio::signal::unix::{SignalKind, signal};
use tracing::Level;

use super::{Context, workspace_error};
use crate::config::LogFormat;
use crate::error::CliError;
use crate::logging;

/// Set to `off`, `create` starts no daemon: for machines where something
/// else keeps disk space, and for test harnesses that must leave no
/// process behind.
pub(super) const DAEMON_ENV: &str = "OPENXET_WORKSPACE_DAEMON";

/// The daemon's log, in the state directory.
const LOG: &str = "daemon.log";

/// Runs the daemon in this process until SIGTERM or SIGINT.
pub(super) async fn run(context: &Context) -> Result<(), CliError> {
    logging::init(LogFormat::Text, Level::INFO);
    let budget = context.budget()?;
    let signal_error = |err: std::io::Error| {
        CliError::new(
            "signal",
            format!("could not install the shutdown signal handlers: {err}"),
            "Check that the process may handle SIGTERM and SIGINT.",
        )
    };
    let mut term = signal(SignalKind::terminate()).map_err(signal_error)?;
    let mut int = signal(SignalKind::interrupt()).map_err(signal_error)?;
    let shutdown = async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    };
    run_daemon(&context.state_dir, budget, shutdown)
        .await
        .map_err(|err| workspace_error(&err))
}

/// Starts the daemon for `state_dir` in the background unless one runs.
/// Returns whether this call started it.
pub(super) fn start(state_dir: &Path) -> Result<bool, String> {
    if daemon_running(state_dir) {
        return Ok(false);
    }
    let exe = std::env::current_exe().map_err(|err| format!("no path to openxet: {err}"))?;
    let log_path = state_dir.join(LOG);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .map_err(|err| format!("could not open {}: {err}", log_path.display()))?;
    Command::new(exe)
        .args(["workspace", "daemon"])
        .env(STATE_DIR_ENV, state_dir)
        // Out of every workspace, so the daemon's own directory never
        // makes one look in use, and out of the terminal's process group,
        // so a Ctrl-C there doesn't stop it.
        .current_dir("/")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|err| format!("could not start the workspace daemon: {err}"))?;
    Ok(true)
}
