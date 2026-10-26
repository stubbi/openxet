//! The real `openxet` binary and a running `openxet serve`, for the
//! command tests that need a server (`purge.rs`, `label.rs`).

// The helpers are test code too; a panic is the intended failure.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

/// How long any one step may take before the test fails instead of hanging.
const PATIENCE: Duration = Duration::from_mins(2);

/// `openxet` with no inherited openxet or Xet configuration and no stdin,
/// so a command that prompts fails instead of hanging.
pub fn openxet() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openxet"));
    for (name, _) in std::env::vars_os() {
        let name_str = name.to_string_lossy();
        if name_str.starts_with("OPENXET_")
            || name_str.starts_with("HF_")
            || name_str.to_ascii_lowercase().ends_with("_proxy")
        {
            cmd.env_remove(name);
        }
    }
    cmd.stdin(Stdio::null());
    cmd
}

/// A running `openxet serve`. Dropping it kills the process.
pub struct Server {
    child: Child,
    pub endpoint: String,
    /// The dev token, or empty when a config sets `[auth]`.
    pub token: String,
    pub data_dir: PathBuf,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_done: Option<thread::JoinHandle<()>>,
}

impl Server {
    /// A server on `data_dir`, with `config` if given.
    pub fn start(data_dir: &Path, config: Option<&Path>) -> Self {
        Self::start_with(data_dir, config, "0", &[])
    }

    /// [`Server::start`] on `port` (`0` for a free one), with `env`.
    pub fn start_with(
        data_dir: &Path,
        config: Option<&Path>,
        port: &str,
        env: &[(&str, &str)],
    ) -> Self {
        let mut cmd = openxet();
        cmd.args(["serve", "--json", "--port", port, "--data-dir"])
            .arg(data_dir)
            .envs(env.iter().copied());
        if let Some(config) = config {
            cmd.arg("--config").arg(config);
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut pipe = child.stderr.take().unwrap();
        let sink = Arc::clone(&stderr);
        let stderr_done = thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        let (tx, rx) = mpsc::channel();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        thread::spawn(move || {
            let mut line = String::new();
            let _ = stdout.read_line(&mut line);
            let _ = tx.send(line);
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        });
        let line = rx.recv_timeout(PATIENCE).unwrap_or_default();
        let Ok(ready) = serde_json::from_str::<Value>(&line) else {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "openxet serve printed no ready line; stderr:\n{}",
                String::from_utf8_lossy(&stderr.lock().unwrap())
            );
        };
        Self {
            child,
            endpoint: ready["endpoint"].as_str().unwrap().to_owned(),
            token: ready["token"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_default(),
            data_dir: data_dir.to_owned(),
            stderr,
            stderr_done: Some(stderr_done),
        }
    }

    /// A client command aimed at this server with `token`, with its own
    /// Xet cache under `cache`.
    pub fn client(&self, token: &str, cache: &Path) -> Command {
        let mut cmd = openxet();
        cmd.env("OPENXET_ENDPOINT", &self.endpoint)
            .env("OPENXET_TOKEN", token)
            .env("HF_XET_CACHE", cache);
        cmd
    }
}

impl Server {
    /// The port the server listens on.
    pub fn port(&self) -> String {
        self.endpoint.rsplit(':').next().unwrap().to_owned()
    }

    /// Stops the server with `SIGTERM`, asserts it exits 0, and returns
    /// what it logged.
    pub fn stop(mut self) -> String {
        let pid = self.child.id().to_string();
        assert!(
            Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .unwrap()
                .success()
        );
        let deadline = Instant::now() + PATIENCE;
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < deadline, "openxet serve did not exit");
            thread::sleep(Duration::from_millis(20));
        };
        if let Some(reader) = self.stderr_done.take() {
            reader.join().unwrap();
        }
        let log = String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned();
        assert_eq!(status.code(), Some(0), "stderr:\n{log}");
        log
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Runs `cmd` to completion, failing the test if it takes too long. Its
/// output is read while it runs, so a command printing more than a pipe
/// holds doesn't block on a full pipe.
pub fn run(cmd: &mut Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let drain = |mut pipe: Box<dyn std::io::Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let stdout = drain(Box::new(child.stdout.take().unwrap()));
    let stderr = drain(Box::new(child.stderr.take().unwrap()));
    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the command did not finish; stderr:\n{}",
                String::from_utf8_lossy(&stderr.join().unwrap())
            );
        }
        thread::sleep(Duration::from_millis(20));
    };
    Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    }
}

/// The one JSON object a successful `--json` run prints, after checking it
/// exited 0.
pub fn json_stdout(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "exit {:?}; stderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not one JSON object ({err}): {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

/// The `openxet.error.v1` object a failed `--json` run prints on stderr,
/// after checking its exit code and that it carries a hint.
pub fn json_error(out: &Output, exit: i32) -> Value {
    let error: Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is not one JSON object ({err}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(out.status.code(), Some(exit), "{error}");
    assert_eq!(error["schema"], "openxet.error.v1", "{error}");
    assert!(
        error["hint"].as_str().is_some_and(|h| !h.is_empty()),
        "{error}"
    );
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    error
}

/// Deterministic bytes that don't compress or dedup within themselves:
/// splitmix64 (Steele, Lea and Flood, 2014).
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len + 8);
    while out.len() < len {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}
