//! `openxet workspace daemon`: the per-user daemon `create` starts in the
//! background (`run`, hidden), and stopping or restarting it, so changed
//! limits apply without hunting for its process.

use std::fmt::Write as _;
use std::fs::{DirBuilder, OpenOptions};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use openxet_workspace::{
    STATE_DIR_ENV, WorkspaceError, daemon_pid, daemon_running, run_daemon, stop_daemon,
};
use serde::Serialize;
use tokio::signal::unix::{SignalKind, signal};
use tracing::Level;

use super::{Context, workspace_error};
use crate::config::LogFormat;
use crate::error::CliError;
use crate::logging;
use crate::output::HumanDisplay;

/// Set to `off`, `create` and `daemon restart` start no daemon: for
/// machines where something else keeps disk space, and for test harnesses
/// that must leave no process behind.
pub(super) const DAEMON_ENV: &str = "OPENXET_WORKSPACE_DAEMON";

/// The daemon's log, in the state directory.
const LOG: &str = "daemon.log";

/// How long `restart` waits for the new daemon to take its lock.
const START_WAIT: Duration = Duration::from_secs(10);

#[derive(Debug, clap::Subcommand)]
pub enum DaemonCommand {
    /// Run the daemon in this process until SIGTERM or SIGINT
    #[command(hide = true)]
    Run,
    /// Stop the running daemon; the next `create` starts it again
    Stop(DaemonArgs),
    /// Stop the running daemon and start a new one, which reads
    /// `~/.config/openxet/workspace.toml` afresh
    Restart(DaemonArgs),
}

#[derive(Debug, clap::Args)]
pub struct DaemonArgs {
    /// Report which daemon would be stopped, and change nothing
    #[arg(long)]
    dry_run: bool,
}

/// The `openxet.workspace.daemon.v1` result.
#[derive(Debug, Serialize)]
pub struct DaemonResult {
    schema: &'static str,
    /// `stop` or `restart`.
    action: &'static str,
    /// The process id of the daemon stopped, or with `dry_run` that would
    /// be; `null` when none was running.
    stopped_pid: Option<u32>,
    /// Whether a new daemon was started (`restart` only).
    started: bool,
    /// The new daemon's process id.
    started_pid: Option<u32>,
    /// `disabled` when `restart` started none because
    /// `OPENXET_WORKSPACE_DAEMON=off`.
    start_skipped: Option<&'static str>,
    dry_run: bool,
}

impl HumanDisplay for DaemonResult {
    fn human(&self) -> String {
        let (stop, none) = if self.dry_run {
            ("Dry run: would stop", "No workspace daemon is running.")
        } else {
            ("Stopped", "No workspace daemon was running.")
        };
        let mut text = match self.stopped_pid {
            Some(pid) => format!("{stop} the workspace daemon (pid {pid})."),
            None => none.to_owned(),
        };
        if self.action == "stop" {
            if !self.dry_run {
                text.push_str(" The next `openxet workspace create` starts it again.");
            }
        } else if self.dry_run {
            text.push_str(" Would start a new one with the limits in your workspace.toml.");
        } else if let Some(pid) = self.started_pid {
            let _ = write!(
                text,
                " Started a new one (pid {pid}) with the limits in your workspace.toml."
            );
        } else if self.start_skipped.is_some() {
            let _ = write!(
                text,
                " Started none, since {DAEMON_ENV}=off; nothing keeps the free-space floor."
            );
        }
        if self.dry_run {
            text.push_str("\nNothing was changed.");
        }
        text
    }
}

/// Runs `daemon stop`, or with `restart` `daemon restart`.
pub(super) async fn control(
    restart: bool,
    args: &DaemonArgs,
    context: &Context,
) -> Result<DaemonResult, CliError> {
    if restart {
        // Before stopping anything: a new daemon would fail on an invalid
        // config, leaving none running.
        context.budget()?;
    }
    let mut result = DaemonResult {
        schema: "openxet.workspace.daemon.v1",
        action: if restart { "restart" } else { "stop" },
        stopped_pid: None,
        started: false,
        started_pid: None,
        start_skipped: None,
        dry_run: args.dry_run,
    };
    if args.dry_run {
        result.stopped_pid = daemon_pid(&context.state_dir);
        return Ok(result);
    }
    result.stopped_pid = stop_daemon(&context.state_dir)
        .await
        .map_err(|err| stop_error(restart, &err))?;
    if !restart {
        return Ok(result);
    }
    if !context.start_daemon {
        result.start_skipped = Some("disabled");
        return Ok(result);
    }
    start(&context.state_dir).map_err(|err| start_error(&err))?;
    let deadline = Instant::now() + START_WAIT;
    loop {
        // Never the stopped daemon's pid, should a killed daemon have left
        // it in the lock file for a moment.
        if let Some(pid) =
            daemon_pid(&context.state_dir).filter(|&pid| Some(pid) != result.stopped_pid)
        {
            result.started = true;
            result.started_pid = Some(pid);
            return Ok(result);
        }
        if Instant::now() >= deadline {
            return Err(start_error(&format!(
                "it did not start within {} s",
                START_WAIT.as_secs()
            )));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The error of a stop that failed. `restart` started no new daemon, and a
/// daemon still finishing a file stops later on its own, so its hint is to
/// restart once it has; until then the old daemon keeps the old limits.
fn stop_error(restart: bool, err: &WorkspaceError) -> CliError {
    let error = workspace_error(err);
    match err {
        WorkspaceError::DaemonStopping { .. } if restart => CliError::new(
            error.code,
            format!("{}; no new daemon was started", error.message),
            "Run `openxet workspace status` until `daemon_running` is false, then run `openxet \
             workspace daemon restart` again to start one with your workspace.toml's limits; \
             until a daemon runs, nothing keeps the free-space floor.",
        ),
        _ if restart => CliError::new(
            error.code,
            format!("{}; no new daemon was started", error.message),
            format!(
                "{} Then run `openxet workspace daemon restart` again.",
                error.hint
            ),
        ),
        _ => error,
    }
}

fn start_error(err: &str) -> CliError {
    CliError::new(
        "daemon_start",
        format!("could not start the workspace daemon: {err}"),
        "Read `daemon.log` in the state directory for why, then run `openxet workspace daemon \
         restart` again; until a daemon runs, nothing keeps the free-space floor.",
    )
}

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
    // Private to the user, as the registry creates it.
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir)
        .map_err(|err| format!("could not create {}: {err}", state_dir.display()))?;
    let log_path = state_dir.join(LOG);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)
        .map_err(|err| format!("could not open {}: {err}", log_path.display()))?;
    Command::new(exe)
        .args(["workspace", "daemon", "run"])
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
