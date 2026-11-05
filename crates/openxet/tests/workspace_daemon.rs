//! `openxet workspace daemon stop` and `restart`, run as the real binary
//! against a real daemon process, in a temp state directory and a temp
//! `HOME`, so no real daemon or registry is touched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir(&home).unwrap();
        Self {
            state: root.join("state"),
            _dir: dir,
            root,
            home,
        }
    }

    fn openxet(&self, args: &[&str]) -> Command {
        let mut cmd = common::openxet();
        cmd.args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("XDG_CONFIG_HOME")
            .env("OPENXET_WORKSPACE_STATE", &self.state);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.openxet(args).output().unwrap()
    }

    /// `openxet workspace daemon run` in the foreground of a child process.
    fn daemon(&self) -> Daemon {
        let child = self
            .openxet(&["workspace", "daemon", "run"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let daemon = Daemon {
            child,
            stop: self.openxet(&["workspace", "daemon", "stop"]),
        };
        self.wait_for_pid(|running| running == Some(pid));
        daemon
    }

    /// The running daemon's pid, as `stop --dry-run` reports it, which reads
    /// no config.
    fn running_pid(&self) -> Option<u32> {
        let planned = self.run(&["workspace", "daemon", "stop", "--dry-run", "--json"]);
        common::json_stdout(&planned)["stopped_pid"]
            .as_u64()
            .map(|pid| u32::try_from(pid).unwrap())
    }

    fn wait_for_pid(&self, done: impl Fn(Option<u32>) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !done(self.running_pid()) {
            assert!(Instant::now() < deadline, "the daemon never got there");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn write_config(&self, text: &str) {
        let config = self.home.join(".config/openxet/workspace.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(config, text).unwrap();
    }
}

/// A daemon this test started, stopped on drop so a failing test leaves no
/// process behind.
struct Daemon {
    child: Child,
    stop: Command,
}

impl Daemon {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn exited(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.success();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // Also stops a daemon `restart` started in its place.
        let _ = self.stop.output();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn json_ok(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    common::json_stdout(out)
}

#[test]
fn stop_stops_the_running_daemon_and_is_idempotent() {
    let setup = Setup::new();
    let mut daemon = setup.daemon();

    let planned = json_ok(&setup.run(&["workspace", "daemon", "stop", "--dry-run", "--json"]));
    assert_eq!(planned["schema"], "openxet.workspace.daemon.v1");
    assert_eq!(planned["stopped_pid"], daemon.pid());
    assert_eq!(planned["dry_run"], true);
    assert_eq!(setup.running_pid(), Some(daemon.pid()));

    let stopped = json_ok(&setup.run(&["workspace", "daemon", "stop", "--json"]));
    assert_eq!(stopped["action"], "stop");
    assert_eq!(stopped["stopped_pid"], daemon.pid());
    assert_eq!(stopped["started"], false);
    assert!(daemon.exited(), "the daemon did not exit cleanly");
    assert_eq!(setup.running_pid(), None);

    let again = json_ok(&setup.run(&["workspace", "daemon", "stop", "--json"]));
    assert_eq!(again["stopped_pid"], Value::Null);
    let human = setup.run(&["workspace", "daemon", "stop"]);
    assert!(
        String::from_utf8_lossy(&human.stdout).contains("No workspace daemon was running"),
        "{}",
        String::from_utf8_lossy(&human.stdout)
    );
}

#[test]
fn restart_replaces_the_daemon_with_one_on_the_new_limits() {
    let setup = Setup::new();
    let mut daemon = setup.daemon();
    setup.write_config("floor = \"1 GiB\"\n");

    let restarted = json_ok(&setup.run(&["workspace", "daemon", "restart", "--json"]));

    assert_eq!(restarted["action"], "restart");
    assert_eq!(restarted["stopped_pid"], daemon.pid());
    assert_eq!(restarted["started"], true);
    assert!(daemon.exited(), "the old daemon did not exit cleanly");
    let new = setup.running_pid().expect("no daemon after the restart");
    assert_ne!(new, daemon.pid());
}

#[test]
fn restart_refuses_an_invalid_config_and_keeps_the_daemon() {
    let setup = Setup::new();
    let daemon = setup.daemon();
    setup.write_config("floor = \"lots\"\n");

    let out = setup.run(&["workspace", "daemon", "restart", "--json"]);

    assert_eq!(out.status.code(), Some(1));
    let error: Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(error["code"], "config");
    assert_eq!(setup.running_pid(), Some(daemon.pid()));
}

#[test]
fn restart_with_the_daemon_off_only_stops_it() {
    let setup = Setup::new();
    let mut daemon = setup.daemon();

    let out = setup
        .openxet(&["workspace", "daemon", "restart", "--json"])
        .env("OPENXET_WORKSPACE_DAEMON", "off")
        .output()
        .unwrap();

    let restarted = json_ok(&out);
    assert_eq!(restarted["stopped_pid"], daemon.pid());
    assert_eq!(restarted["started"], false);
    assert_eq!(restarted["start_skipped"], "disabled");
    assert!(daemon.exited());
    assert_eq!(setup.running_pid(), None);
}

/// No daemon state is created by a stop with nothing to stop.
#[test]
fn stop_without_a_state_directory_creates_none() {
    let setup = Setup::new();
    let out = json_ok(&setup.run(&["workspace", "daemon", "stop", "--json"]));
    assert_eq!(out["stopped_pid"], Value::Null);
    assert!(!Path::new(&setup.state).exists());
}
