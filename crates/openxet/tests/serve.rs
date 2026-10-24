//! `openxet serve`, `openxet config check` and `openxet token`, run as the
//! real binary (`docs/reference/config.md`, `docs/reference/cli.md`).
//!
//! Servers listen on port 0 and announce themselves with one
//! `openxet.serve.v1` line on stdout once ready, so no test waits on a timer
//! to know the server is up.

// The helpers are test code too; a panic is the intended failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, ExitStatus, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

const ZERO: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// The `hf_xet` 1.7.0 capture (`testdata/hf-xet-1.7.0-upload`): one xorb, and
/// a shard registering the empty file, `a.bin` and `b.bin` over it.
const CAPTURE_XORB: &str = "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c";
const CAPTURE_FILE: &str = "65fef04da9e2708da8ca1acc831962d0686875eff24218d24e108362eeb420df";

/// How long any one step may take before the test fails instead of hanging.
const PATIENCE: Duration = Duration::from_mins(1);

fn testdata(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/hf-xet-1.7.0-upload")
            .join(name),
    )
    .unwrap()
}

fn capture_xorb() -> Vec<u8> {
    testdata(&format!("{CAPTURE_XORB}.xorb"))
}

fn capture_shard() -> Vec<u8> {
    testdata("upload.shard")
}

/// `openxet` with no inherited configuration: every `OPENXET_<SECTION>__<KEY>`
/// variable of the test's own environment is removed.
fn openxet() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openxet"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("OPENXET_") {
            cmd.env_remove(name);
        }
    }
    cmd.stdin(Stdio::null());
    cmd
}

/// The `Authorization` value for `token`. The scheme is a constant so the
/// repository's secret scanner, which flags any literal bearer value, can
/// tell test tokens from leaks.
fn bearer(token: &str) -> String {
    const SCHEME: &str = "Bearer";
    format!("{SCHEME} {token}")
}

/// A running `openxet serve`. Dropping it kills the process.
struct Server {
    child: Option<Child>,
    /// The `openxet.serve.v1` line.
    line: Value,
    endpoint: String,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_done: Option<thread::JoinHandle<()>>,
    _stdout: Arc<Mutex<Option<BufReader<ChildStdout>>>>,
}

impl Server {
    /// Starts `openxet serve --json --port 0 --data-dir data_dir extra…`
    /// with `env`, and waits for its ready line.
    fn start(data_dir: &Path, extra: &[&str], env: &[(&str, &str)]) -> Self {
        let mut cmd = openxet();
        cmd.args(["serve", "--json", "--port", "0", "--data-dir"])
            .arg(data_dir)
            .args(extra)
            .envs(env.iter().copied())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self::spawn(cmd)
    }

    /// Starts a dev-mode server on `port`.
    fn start_on(data_dir: &Path, port: &str) -> Self {
        let mut cmd = openxet();
        cmd.args(["serve", "--json", "--port", port, "--data-dir"])
            .arg(data_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Self::spawn(cmd)
    }

    fn spawn(mut cmd: Command) -> Self {
        let mut child = cmd.spawn().unwrap();
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
            let read = stdout.read_line(&mut line);
            let _ = tx.send((read.map(|_| line), stdout));
        });
        let (line, stdout) = match rx.recv_timeout(PATIENCE) {
            Ok((Ok(line), stdout)) if !line.is_empty() => (line, stdout),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stderr_done.join();
                panic!(
                    "openxet serve printed no ready line; stderr:\n{}",
                    String::from_utf8_lossy(&stderr.lock().unwrap())
                );
            }
        };
        let line: Value = serde_json::from_str(&line)
            .unwrap_or_else(|err| panic!("the ready line is not JSON ({err}): {line}"));
        let endpoint = line["endpoint"].as_str().unwrap().to_owned();
        Self {
            child: Some(child),
            line,
            endpoint,
            stderr,
            stderr_done: Some(stderr_done),
            _stdout: Arc::new(Mutex::new(Some(stdout))),
        }
    }

    fn token(&self) -> String {
        self.line["token"].as_str().unwrap().to_owned()
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.endpoint)
    }

    fn addr(&self) -> SocketAddr {
        self.endpoint.trim_start_matches("http://").parse().unwrap()
    }

    fn pid(&self) -> String {
        self.child.as_ref().unwrap().id().to_string()
    }

    fn signal(&self, signal: &str) {
        let status = Command::new("kill")
            .args([signal, &self.pid()])
            .status()
            .unwrap();
        assert!(status.success());
    }

    /// Waits for the process to exit and returns its status and stderr.
    fn wait(mut self) -> (ExitStatus, String) {
        let mut child = self.child.take().unwrap();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(child.wait());
        });
        let status = rx
            .recv_timeout(PATIENCE)
            .expect("openxet serve did not exit")
            .unwrap();
        self.stderr_done.take().unwrap().join().unwrap();
        let stderr = String::from_utf8_lossy(&self.stderr.lock().unwrap()).into_owned();
        (status, stderr)
    }

    /// Stops the server with `SIGTERM` and asserts a clean exit.
    fn stop(self) -> String {
        self.signal("-TERM");
        let (status, stderr) = self.wait();
        assert_eq!(status.code(), Some(0), "stderr:\n{stderr}");
        stderr
    }

    /// Kills the server with `SIGKILL`: no shutdown code runs.
    fn kill(self) {
        self.signal("-KILL");
        let (status, _) = self.wait();
        assert!(!status.success());
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(PATIENCE)
        .build()
        .unwrap()
}

async fn status(request: reqwest::RequestBuilder) -> u16 {
    request.send().await.unwrap().status().as_u16()
}

async fn upload_xorb(server: &Server, token: &str) -> u16 {
    status(
        http()
            .post(server.url(&format!("/v1/xorbs/default/{CAPTURE_XORB}")))
            .header("authorization", bearer(token))
            .body(capture_xorb()),
    )
    .await
}

async fn upload_shard(server: &Server, token: &str) -> u16 {
    status(
        http()
            .post(server.url("/v1/shards"))
            .header("authorization", bearer(token))
            .body(capture_shard()),
    )
    .await
}

/// The v1 reconstruction of the capture's `a.bin`: its `(url, start, end)`
/// fetch entries.
async fn transfer_urls(server: &Server, token: &str) -> Vec<(String, u64, u64)> {
    let response = http()
        .get(server.url(&format!("/v1/reconstructions/{CAPTURE_FILE}")))
        .header("authorization", bearer(token))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let answer: Value = response.json().await.unwrap();
    let mut urls = Vec::new();
    for entries in answer["fetch_info"].as_object().unwrap().values() {
        for entry in entries.as_array().unwrap() {
            urls.push((
                entry["url"].as_str().unwrap().to_owned(),
                entry["url_range"]["start"].as_u64().unwrap(),
                entry["url_range"]["end"].as_u64().unwrap(),
            ));
        }
    }
    assert!(!urls.is_empty());
    urls
}

async fn fetch(url: &str, start: u64, end: u64) -> (u16, Vec<u8>) {
    let response = http()
        .get(url)
        .header("range", format!("bytes={start}-{end}"))
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.bytes().await.unwrap().to_vec())
}

fn is_oxt_token(token: &str) -> bool {
    token.len() == 47
        && token.starts_with("oxt_")
        && token[4..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn write_config(dir: &Path, toml: &str) -> PathBuf {
    let path = dir.join("openxet.toml");
    std::fs::write(&path, toml).unwrap();
    path
}

/// Runs `cmd` to completion. A command still running after [`PATIENCE`]
/// (a server that started when it should have failed) is killed, so a
/// failing test never hangs.
fn run(cmd: &mut Command) -> Output {
    let child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let pid = child.id().to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    if let Ok(out) = rx.recv_timeout(PATIENCE) {
        out.unwrap()
    } else {
        let _ = Command::new("kill").args(["-9", &pid]).status();
        panic!("the command was still running after {PATIENCE:?}");
    }
}

fn json_stdout(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not one JSON object ({err}): {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn json_stderr(out: &Output) -> Value {
    let error: Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is not one JSON object ({err}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(error["schema"], "openxet.error.v1");
    error
}

/// The `openxet.error.v1` object a failed `serve` ends its stderr with.
/// `serve` logs to stderr too (cli.md), so a log line may come first: on a
/// slow CI disk, sqlx warned of a 1 s `PRAGMA journal_mode = WAL` before
/// the error. The last line is the error, and no other line may be one.
fn serve_error(out: &Output) -> Value {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut lines = stderr.lines().filter(|l| !l.trim().is_empty());
    let last = lines.next_back().unwrap_or_default();
    let error: Value = serde_json::from_str(last)
        .unwrap_or_else(|err| panic!("stderr does not end with one JSON object ({err}): {stderr}"));
    assert_eq!(error["schema"], "openxet.error.v1");
    assert!(
        !lines.any(|l| l.contains("openxet.error.v1")),
        "more than one error on stderr: {stderr}"
    );
    error
}

#[tokio::test]
async fn dev_mode_prints_working_token() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let server = Server::start(&data, &[], &[]);

    let line = &server.line;
    assert_eq!(line["schema"], "openxet.serve.v1");
    assert_eq!(line["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(line["scope"], "default");
    assert_eq!(line["mode"], "write");
    assert_eq!(line["token_exp"].as_u64(), Some(1 << 62));
    assert_eq!(line["data_dir"].as_str(), Some(data.to_str().unwrap()));
    let token = server.token();
    assert!(is_oxt_token(&token), "{token}");
    assert_ne!(server.addr().port(), 0);

    let file = server.url(&format!("/v1/files/{ZERO}"));
    let authorized = http().head(&file).header("authorization", bearer(&token));
    assert_eq!(status(authorized).await, 404);
    assert_eq!(status(http().head(&file)).await, 401);
    assert_eq!(status(http().get(server.url("/healthz"))).await, 200);
    assert_eq!(status(http().get(server.url("/readyz"))).await, 200);

    for name in ["dev-token", "transfer.key", "index.sqlite"] {
        assert!(data.join(name).is_file(), "{name} is missing");
    }
    assert!(data.join("bucket").is_dir());
    assert_eq!(
        std::fs::read_to_string(data.join("dev-token"))
            .unwrap()
            .trim(),
        token
    );
    #[cfg(unix)]
    for name in ["dev-token", "transfer.key"] {
        assert_eq!(mode_of(&data.join(name)), 0o600, "{name}");
    }

    let stderr = server.stop();
    assert!(!stderr.contains(&token), "the dev token reached the log");
}

#[tokio::test]
async fn dev_mode_banner_is_ready_to_paste() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = openxet();
    cmd.args(["serve", "--port", "0", "--data-dir"])
        .arg(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut banner = String::new();
        for line in stdout.lines() {
            let Ok(line) = line else { break };
            banner.push_str(&line);
            banner.push('\n');
            if line.starts_with("Stop with") {
                break;
            }
        }
        let _ = tx.send(banner);
    });
    let banner = rx.recv_timeout(PATIENCE).unwrap();
    let _ = child.kill();
    let _ = child.wait();

    let token = std::fs::read_to_string(dir.path().join("dev-token"))
        .unwrap()
        .trim()
        .to_owned();
    let endpoint = banner
        .lines()
        .find_map(|l| l.trim().strip_prefix("endpoint"))
        .unwrap()
        .trim()
        .to_owned();
    assert!(endpoint.starts_with("http://127.0.0.1:"), "{banner}");
    assert!(banner.contains(&token), "{banner}");
    assert!(banner.contains("scope \"default\" (write)"), "{banner}");
    // Stock macOS and Debian have `python3`, not always `python`.
    assert!(banner.contains("pip install hf_xet"), "{banner}");
    assert!(banner.contains("  python3 -c 'import hf_xet;"), "{banner}");
    // hf_xet 1.7.0: upload_files(file_paths, endpoint, token_info,
    // token_refresher, progress_updater, _repo_type), token_info = (token, exp).
    assert!(
        banner.contains(&format!(
            "hf_xet.upload_files([\"model.bin\"], \"{endpoint}\", (\"{token}\", 2**62), None, None, None)"
        )),
        "{banner}"
    );
    assert!(
        banner.contains(&format!("OPENXET_ENDPOINT={endpoint}")),
        "{banner}"
    );
    assert!(
        banner.contains(&format!("OPENXET_TOKEN={token}")),
        "{banner}"
    );
    assert!(banner.contains("openxet upload model.bin"), "{banner}");
}

#[tokio::test]
async fn restart_keeps_data_token_and_urls() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), &[], &[]);
    let token = server.token();
    assert_eq!(upload_xorb(&server, &token).await, 200);
    assert_eq!(upload_shard(&server, &token).await, 200);
    let urls = transfer_urls(&server, &token).await;
    let mut before = Vec::new();
    for (url, start, end) in &urls {
        let (code, bytes) = fetch(url, *start, *end).await;
        assert_eq!(code, 206);
        before.push(bytes);
    }
    let key = std::fs::read(dir.path().join("transfer.key")).unwrap();
    // The URLs name this port, so the restarted server listens on it again.
    let port = server.addr().port().to_string();
    server.stop();

    let server = Server::start_on(dir.path(), &port);
    assert_eq!(server.token(), token, "the dev token changed on restart");
    assert_eq!(
        std::fs::read(dir.path().join("transfer.key")).unwrap(),
        key,
        "the transfer key changed on restart"
    );
    for ((url, start, end), bytes) in urls.iter().zip(&before) {
        let (code, after) = fetch(url, *start, *end).await;
        assert_eq!(code, 206, "{url}");
        assert_eq!(&after, bytes);
    }
    let head = http()
        .head(server.url(&format!("/v1/files/{CAPTURE_FILE}")))
        .header("authorization", bearer(&token));
    assert_eq!(status(head).await, 200);
    assert_eq!(transfer_urls(&server, &token).await.len(), urls.len());
}

#[tokio::test]
async fn acked_write_survives_sigkill() {
    // Design §5.9 req 7: an acknowledged write survives a crash.
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), &[], &[]);
    let token = server.token();
    assert_eq!(upload_xorb(&server, &token).await, 200);
    server.kill();

    let server = Server::start(dir.path(), &[], &[]);
    let head = http()
        .head(server.url(&format!("/v1/xorbs/default/{CAPTURE_XORB}")))
        .header("authorization", bearer(&token));
    assert_eq!(status(head).await, 200, "the acknowledged xorb was lost");
    assert_eq!(upload_shard(&server, &token).await, 200);
    server.kill();

    let server = Server::start(dir.path(), &[], &[]);
    let head = http()
        .head(server.url(&format!("/v1/files/{CAPTURE_FILE}")))
        .header("authorization", bearer(&token));
    assert_eq!(status(head).await, 200, "the acknowledged shard was lost");
    for (url, start, end) in transfer_urls(&server, &token).await {
        assert_eq!(fetch(&url, start, end).await.0, 206);
    }
}

#[test]
fn port_in_use_is_actionable() {
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port().to_string();
    let dir = tempfile::tempdir().unwrap();

    let out = run(openxet()
        .args(["serve", "--json", "--port", &port, "--data-dir"])
        .arg(dir.path()));
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let error = serve_error(&out);
    assert_eq!(error["code"], "address_in_use");
    assert!(error["message"].as_str().unwrap().contains(&port));
    assert!(error["hint"].as_str().unwrap().contains("--port"));

    let out = run(openxet()
        .args(["serve", "--port", &port, "--data-dir"])
        .arg(dir.path()));
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--port"));
}

#[test]
fn config_env_override() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(
        dir.path(),
        "[cas]\nurl_lifetime = \"2h\"\ndedup_visibility = \"global\"\n",
    );

    let out = run(openxet()
        .args(["config", "check", "--json", "--config"])
        .arg(&config)
        .env("OPENXET_CAS__DEDUP_VISIBILITY", "per-scope")
        .env("OPENXET_SERVER__LOG_FORMAT", "json"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let report = json_stdout(&out);
    assert_eq!(report["schema"], "openxet.config-check.v1");
    assert_eq!(report["valid"], true);
    let setting = |key: &str| {
        report["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["key"] == key)
            .unwrap_or_else(|| panic!("{key} is not reported: {report}"))
            .clone()
    };
    assert_eq!(setting("cas.dedup_visibility")["value"], "per-scope");
    assert_eq!(
        setting("cas.dedup_visibility")["source"],
        "env OPENXET_CAS__DEDUP_VISIBILITY"
    );
    assert_eq!(setting("cas.url_lifetime")["value"], "2h");
    assert!(
        setting("cas.url_lifetime")["source"]
            .as_str()
            .unwrap()
            .starts_with("file ")
    );
    assert_eq!(setting("server.log_format")["value"], "json");
    assert_eq!(setting("server.header_read_timeout")["source"], "default");

    // A server started with an override uses it: here it listens on the
    // override's port and logs JSON.
    let data = dir.path().join("data");
    let server = Server::start(
        &data,
        &["--config", config.to_str().unwrap()],
        &[("OPENXET_SERVER__LOG_FORMAT", "json")],
    );
    let stderr = server.stop();
    let first = stderr.lines().next().unwrap();
    assert!(
        serde_json::from_str::<Value>(first).is_ok(),
        "the log is not JSON: {first}"
    );
}

#[test]
fn config_check_explains_bad_value() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(
        dir.path(),
        r#"
[server]
public_url = "http://cas.example.com/xet?x=1"
header_read_timeout = "soon"
porrt = 8080

[cas]
url_lifetime = "5m"
heartbeat_interval = "0s"
dedup_visibility = "everyone"
"#,
    );

    let out = run(openxet()
        .args(["config", "check", "--json", "--config"])
        .arg(&config)
        .env("OPENXET_AUTH__MAX_TOKEN_LIFETIME", "90d"));
    assert_eq!(out.status.code(), Some(1));
    let report = json_stdout(&out);
    assert_eq!(report["schema"], "openxet.config-check.v1");
    assert_eq!(report["valid"], false);
    let problems = report["problems"].as_array().unwrap();
    let keys: Vec<&str> = problems
        .iter()
        .map(|p| p["key"].as_str().unwrap())
        .collect();
    for key in [
        "server.public_url",
        "server.header_read_timeout",
        "server.porrt",
        "cas.url_lifetime",
        "cas.heartbeat_interval",
        "cas.dedup_visibility",
        "auth.max_token_lifetime",
    ] {
        assert!(keys.contains(&key), "{key} is not reported: {keys:?}");
    }
    for problem in problems {
        for field in ["key", "source", "rule", "hint"] {
            assert!(
                problem[field].as_str().is_some_and(|s| !s.is_empty()),
                "{field} missing in {problem}"
            );
        }
    }
    let lifetime = problems
        .iter()
        .find(|p| p["key"] == "cas.url_lifetime")
        .unwrap();
    assert_eq!(lifetime["value"], "5m");
    assert!(lifetime["rule"].as_str().unwrap().contains("30m"));
    let max = problems
        .iter()
        .find(|p| p["key"] == "auth.max_token_lifetime")
        .unwrap();
    assert_eq!(max["source"], "env OPENXET_AUTH__MAX_TOKEN_LIFETIME");

    // Without --json, each problem is readable on its own lines.
    let out = run(openxet().args(["config", "check", "--config"]).arg(&config));
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("server.public_url"), "{text}");
    assert!(text.contains("origin root"), "{text}");
    assert!(text.contains("hint:"), "{text}");

    // `serve` refuses the same file, pointing at `config check`.
    let out = run(openxet()
        .args(["serve", "--json", "--port", "0", "--config"])
        .arg(&config));
    assert_eq!(out.status.code(), Some(1));
    let error = serve_error(&out);
    assert_eq!(error["code"], "config");
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("openxet config check")
    );
}

#[test]
fn config_check_rejects_bad_public_urls() {
    let dir = tempfile::tempdir().unwrap();
    for url in [
        "ftp://cas.example.com",
        "cas.example.com",
        "http://cas.example.com/v1",
        "https://cas.example.com/#top",
        "https://user:pw@cas.example.com",
    ] {
        let config = write_config(dir.path(), &format!("[server]\npublic_url = \"{url}\"\n"));
        let out = run(openxet()
            .args(["config", "check", "--json", "--config"])
            .arg(&config));
        assert_eq!(out.status.code(), Some(1), "{url} was accepted");
        let report = json_stdout(&out);
        assert_eq!(report["problems"][0]["key"], "server.public_url", "{url}");
    }
    for url in ["http://127.0.0.1:8080", "https://cas.example.com/"] {
        let config = write_config(dir.path(), &format!("[server]\npublic_url = \"{url}\"\n"));
        let out = run(openxet()
            .args(["config", "check", "--json", "--config"])
            .arg(&config));
        assert_eq!(out.status.code(), Some(0), "{url}: {out:?}");
    }
}

#[tokio::test]
async fn token_mint_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("signing.key");

    let out = run(openxet()
        .args(["token", "keygen", "--json", "--key"])
        .arg(&key));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let keygen = json_stdout(&out);
    assert_eq!(keygen["schema"], "openxet.token-keygen.v1");
    let public = PathBuf::from(keygen["public_key"].as_str().unwrap());
    assert!(
        std::fs::read_to_string(&public)
            .unwrap()
            .starts_with("-----BEGIN PUBLIC KEY-----")
    );
    #[cfg(unix)]
    assert_eq!(mode_of(&key), 0o600);
    // A second keygen never overwrites the key.
    let out = run(openxet()
        .args(["token", "keygen", "--json", "--key"])
        .arg(&key));
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_stderr(&out)["code"], "exists");

    let mint = |mode: &str, ttl: &str| {
        run(openxet()
            .args([
                "token",
                "mint",
                "--json",
                "--scope",
                "team/models",
                "--mode",
                mode,
            ])
            .args(["--ttl", ttl, "--key"])
            .arg(&key))
    };
    let out = mint("read", "1h");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let minted = json_stdout(&out);
    assert_eq!(minted["schema"], "openxet.token-mint.v1");
    assert_eq!(minted["kid"], keygen["kid"]);
    assert_eq!(minted["scope"], "team/models");
    assert_eq!(minted["mode"], "read");
    let exp = minted["exp"].as_u64().unwrap();
    assert_eq!(exp - minted["iat"].as_u64().unwrap(), 3600);
    let read_token = minted["token"].as_str().unwrap().to_owned();
    let write_token = json_stdout(&mint("write", "10m"))["token"]
        .as_str()
        .unwrap()
        .to_owned();

    let out = mint("read", "31d");
    assert_eq!(out.status.code(), Some(1));
    let error = json_stderr(&out);
    assert_eq!(error["code"], "invalid_ttl");
    assert!(error["hint"].as_str().unwrap().contains("30d"));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("--ttl 31d is outside 1m to 30d"),
        "{error}"
    );

    let config = write_config(
        dir.path(),
        &format!("[auth]\njwt_public_keys = [\"{}\"]\n", public.display()),
    );
    let server = Server::start(
        &dir.path().join("data"),
        &["--config", config.to_str().unwrap()],
        &[],
    );
    assert!(server.line["token"].is_null(), "{}", server.line);
    let head = |token: &str| {
        http()
            .head(server.url(&format!("/v1/files/{ZERO}")))
            .header("authorization", bearer(token))
    };
    assert_eq!(status(head(&read_token)).await, 404);
    assert_eq!(status(head(&write_token)).await, 404);
    assert_eq!(status(head("oxt_not-a-configured-token")).await, 401);
    assert_eq!(upload_xorb(&server, &read_token).await, 403);
    assert_eq!(upload_xorb(&server, &write_token).await, 200);
}

#[tokio::test]
async fn token_generate_configures_a_static_token() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(openxet().args([
        "token",
        "generate",
        "--json",
        "--scope",
        "ci",
        "--mode",
        "write",
        "--subject",
        "ci-bot",
    ]));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let generated = json_stdout(&out);
    assert_eq!(generated["schema"], "openxet.token-generate.v1");
    let token = generated["token"].as_str().unwrap().to_owned();
    assert!(is_oxt_token(&token), "{token}");
    let toml = generated["config"].as_str().unwrap();
    assert!(!toml.contains(&token), "the config snippet holds the token");

    let config = write_config(dir.path(), toml);
    let server = Server::start(
        &dir.path().join("data"),
        &["--config", config.to_str().unwrap()],
        &[],
    );
    assert_eq!(upload_xorb(&server, &token).await, 200);
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one session of every credential kind, then one assertion over its log"
)]
async fn credentials_never_logged() {
    // Design §5.9: credentials are never echoed or logged, at any level.
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("signing.key");
    let keygen = json_stdout(&run(openxet()
        .args(["token", "keygen", "--json", "--key"])
        .arg(&key)));
    let generate = |mode: &str| {
        json_stdout(&run(openxet().args([
            "token",
            "generate",
            "--json",
            "--scope",
            "logs",
            "--mode",
            mode,
            "--subject",
            "logs",
        ])))
    };
    let (write, read) = (generate("write"), generate("read"));
    let mint = json_stdout(&run(openxet()
        .args([
            "token", "mint", "--json", "--scope", "logs", "--mode", "write",
        ])
        .args(["--ttl", "1h", "--key"])
        .arg(&key)));
    let write_token = write["token"].as_str().unwrap().to_owned();
    let read_token = read["token"].as_str().unwrap().to_owned();
    let jwt = mint["token"].as_str().unwrap().to_owned();
    let config = write_config(
        dir.path(),
        &format!(
            "{}\n{}\n[auth]\njwt_public_keys = [\"{}\"]\n",
            write["config"].as_str().unwrap(),
            read["config"].as_str().unwrap(),
            keygen["public_key"].as_str().unwrap()
        ),
    );

    let server = Server::start(
        &dir.path().join("data"),
        &["--config", config.to_str().unwrap()],
        &[("OPENXET_SERVER__LOG_LEVEL", "trace")],
    );
    let mut secrets = vec![write_token.clone(), read_token.clone(), jwt.clone()];
    secrets.push(
        std::fs::read_to_string(dir.path().join("data/transfer.key"))
            .unwrap()
            .trim()
            .to_owned(),
    );
    let private_pem = std::fs::read_to_string(&key).unwrap();
    secrets.extend(
        private_pem
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .map(str::to_owned),
    );

    // Upload, download and every kind of refusal.
    assert_eq!(upload_xorb(&server, &jwt).await, 200);
    assert_eq!(upload_shard(&server, &write_token).await, 200);
    assert_eq!(upload_xorb(&server, &read_token).await, 403);
    let unknown = format!("{write_token}x");
    secrets.push(unknown.clone());
    assert_eq!(upload_xorb(&server, &unknown).await, 401);
    let sig_start = jwt.rfind('.').unwrap() + 1;
    let swap = if jwt[sig_start..].starts_with('A') {
        "B"
    } else {
        "A"
    };
    let tampered = format!("{}{swap}{}", &jwt[..sig_start], &jwt[sig_start + 1..]);
    secrets.push(tampered.clone());
    assert_eq!(upload_xorb(&server, &tampered).await, 401);
    for (url, start, end) in transfer_urls(&server, &read_token).await {
        let sig = reqwest::Url::parse(&url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "sig")
            .unwrap()
            .1
            .into_owned();
        secrets.push(sig);
        assert_eq!(fetch(&url, start, end).await.0, 206);
        assert_eq!(fetch(&url, start, end + 1).await.0, 403);
        let forged = url.replace("sig=", "sig=A");
        assert_eq!(fetch(&forged, start, end).await.0, 403);
    }
    // A credential pasted into a path is never echoed either.
    let path = server.url(&format!("/v1/files/{write_token}"));
    assert_eq!(
        status(
            http()
                .head(path)
                .header("authorization", bearer(&read_token))
        )
        .await,
        400
    );

    let stderr = server.stop();
    assert!(stderr.contains("request"), "nothing was logged: {stderr}");
    for secret in &secrets {
        assert!(
            !stderr.contains(secret.as_str()),
            "a credential reached the log"
        );
    }
}

#[test]
fn sigterm_lets_in_flight_requests_finish() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), &[], &[("OPENXET_SERVER__LOG_LEVEL", "debug")]);
    let (mut stream, tail) = start_upload(&server);
    server.wait_for_log(UPLOAD_STARTED);
    server.signal("-TERM");
    wait_until_refusing(server.addr());

    stream.write_all(&tail).unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).unwrap();
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    let (status, stderr) = server.wait();
    assert_eq!(status.code(), Some(0), "{stderr}");
}

/// The debug log line of a request whose handler has started, so its
/// connection is in flight rather than idle or still in the listen backlog.
const UPLOAD_STARTED: &str = "started processing request";

impl Server {
    /// Waits until the server's log holds `needle`.
    fn wait_for_log(&self, needle: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !String::from_utf8_lossy(&self.stderr.lock().unwrap()).contains(needle) {
            assert!(Instant::now() < deadline, "the log never showed {needle:?}");
            thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Starts sending a xorb upload's first half and returns the connection.
fn start_upload(server: &Server) -> (TcpStream, Vec<u8>) {
    let body = capture_xorb();
    let (head, tail) = body.split_at(body.len() / 2);
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    write!(
        stream,
        "POST /v1/xorbs/default/{CAPTURE_XORB} HTTP/1.1\r\nHost: x\r\nAuthorization: {}\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        bearer(&server.token()),
        body.len()
    )
    .unwrap();
    stream.write_all(head).unwrap();
    stream.flush().unwrap();
    (stream, tail.to_vec())
}

/// Waits until the server has stopped accepting: new connections are
/// refused once its listener is closed.
fn wait_until_refusing(addr: SocketAddr) {
    let deadline = Instant::now() + PATIENCE;
    while TcpStream::connect(addr).is_ok() {
        assert!(Instant::now() < deadline, "the server kept accepting");
        thread::yield_now();
    }
}

#[test]
fn a_second_signal_exits_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), &[], &[("OPENXET_SERVER__LOG_LEVEL", "debug")]);
    let (_upload, _) = start_upload(&server);
    server.wait_for_log(UPLOAD_STARTED);
    server.signal("-INT");
    wait_until_refusing(server.addr());
    server.signal("-INT");
    let (status, stderr) = server.wait();
    assert_eq!(status.code(), Some(1), "{stderr}");
}

#[test]
fn slow_request_headers_are_cut_off() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(
        dir.path(),
        &[],
        &[("OPENXET_SERVER__HEADER_READ_TIMEOUT", "1s")],
    );
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    let started = Instant::now();
    stream
        .write_all(b"GET /healthz HTTP/1.1\r\nHost: x\r\n")
        .unwrap();
    let mut answer = Vec::new();
    // The server closes the connection; a read timeout would fail here.
    let _ = stream.read_to_end(&mut answer);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "the connection stayed open"
    );
    assert!(!String::from_utf8_lossy(&answer).contains("200 OK"));
}

#[test]
fn slow_request_bodies_are_cut_off() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(
        dir.path(),
        &[],
        &[("OPENXET_SERVER__BODY_READ_TIMEOUT", "1s")],
    );
    let mut stream = TcpStream::connect(server.addr()).unwrap();
    stream.set_read_timeout(Some(PATIENCE)).unwrap();
    // A refused upload's body is drained (ruling M1-R22); the deadline
    // bounds that too.
    write!(
        stream,
        "POST /v1/xorbs/default/{CAPTURE_XORB} HTTP/1.1\r\nHost: x\r\nContent-Length: 1000\r\n\r\n"
    )
    .unwrap();
    stream.write_all(&[0; 10]).unwrap();
    let mut answer = String::new();
    let _ = stream.read_to_string(&mut answer);
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
}

#[test]
fn unspecified_bind_needs_a_public_url() {
    let out = run(openxet()
        .args(["config", "check", "--json"])
        .env("OPENXET_SERVER__BIND", "0.0.0.0:8080"));
    assert_eq!(out.status.code(), Some(1));
    let report = json_stdout(&out);
    assert_eq!(report["problems"][0]["key"], "server.public_url");
}

#[test]
fn config_check_never_echoes_secrets() {
    let dir = tempfile::tempdir().unwrap();
    let pasted = json_stdout(&run(openxet().args([
        "token",
        "generate",
        "--json",
        "--mode",
        "write",
        "--subject",
        "a",
    ])))["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let in_env = json_stdout(&run(openxet().args([
        "token",
        "generate",
        "--json",
        "--mode",
        "read",
        "--subject",
        "b",
    ])))["token"]
        .as_str()
        .unwrap()
        .to_owned();
    // The likeliest mistake: the token itself where its hash belongs.
    let config = write_config(
        dir.path(),
        &format!(
            "[[auth.static_tokens]]\nsha256 = \"{pasted}\"\nscope = \"s\"\nmode = \"write\"\n\
             subject = \"x\"\n"
        ),
    );
    let env_value = format!("[{{sha256 = \"{in_env}\""); // not valid TOML
    for json in [true, false] {
        let mut cmd = openxet();
        cmd.args(["config", "check", "--config"]).arg(&config);
        if json {
            cmd.arg("--json");
        }
        let out = run(cmd.env("OPENXET_AUTH__STATIC_TOKENS", &env_value));
        assert_eq!(out.status.code(), Some(1));
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        for secret in [&pasted, &in_env] {
            assert!(!all.contains(secret.as_str()), "a token was echoed: {all}");
        }
        assert!(all.contains("<redacted>"), "{all}");
        assert!(all.contains("looks like a raw token"), "{all}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_data_dir_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let server = Server::start(&data, &[], &[]);
    let token = server.token();
    assert_eq!(upload_xorb(&server, &token).await, 200);
    assert_eq!(upload_shard(&server, &token).await, 200);
    server.stop();

    let mut dirs = vec![data.clone()];
    let mut files = 0;
    while let Some(dir) = dirs.pop() {
        assert_eq!(mode_of(&dir), 0o700, "{}", dir.display());
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                assert_eq!(mode_of(&path), 0o600, "{}", path.display());
                files += 1;
            }
        }
    }
    assert!(files >= 5, "index files, keys, a xorb and a shard");
}

#[cfg(unix)]
#[test]
fn a_shared_data_dir_is_warned_about() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = Server::start(&data, &[], &[]);
    let stderr = server.stop();
    assert!(
        stderr.contains(&format!("chmod 700 {}", data.display())),
        "{stderr}"
    );
}

#[test]
fn configured_mode_generates_the_default_transfer_key_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let generated = json_stdout(&run(openxet().args([
        "token",
        "generate",
        "--json",
        "--mode",
        "write",
        "--subject",
        "a",
    ])));
    let config = write_config(dir.path(), generated["config"].as_str().unwrap());
    let data = dir.path().join("data");
    let server = Server::start(&data, &["--config", config.to_str().unwrap()], &[]);
    let stderr = server.stop();
    assert!(data.join("transfer.key").is_file());
    assert!(stderr.contains("every node"), "{stderr}");
    assert!(
        stderr.contains(data.join("transfer.key").to_str().unwrap()),
        "{stderr}"
    );
}

#[test]
fn an_explicit_transfer_key_file_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("keys/transfer.key");
    let config = write_config(
        dir.path(),
        &format!("[cas]\ntransfer_key_file = \"{}\"\n", missing.display()),
    );
    let out = run(openxet()
        .args(["serve", "--json", "--port", "0", "--data-dir"])
        .arg(dir.path().join("data"))
        .arg("--config")
        .arg(&config));
    assert_eq!(out.status.code(), Some(1));
    let error = serve_error(&out);
    assert_eq!(error["code"], "no_key");
    assert!(
        error["hint"]
            .as_str()
            .unwrap()
            .contains("openssl rand -hex 32"),
        "{error}"
    );
    assert!(!missing.exists(), "the key was generated anyway");
}

#[test]
fn mint_with_the_public_key_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("signing.key");
    let keygen = json_stdout(&run(openxet()
        .args(["token", "keygen", "--json", "--key"])
        .arg(&key)));
    let out = run(openxet()
        .args([
            "token", "mint", "--json", "--scope", "s", "--mode", "read", "--key",
        ])
        .arg(keygen["public_key"].as_str().unwrap()));
    assert_eq!(out.status.code(), Some(1));
    let error = json_stderr(&out);
    assert_eq!(error["code"], "invalid_key", "{error}");
    assert!(
        error["hint"].as_str().unwrap().contains("public key"),
        "{error}"
    );
}

#[test]
fn keygen_defaults_to_the_config_directory() {
    let dir = tempfile::tempdir().unwrap();
    let out = run(openxet()
        .args(["token", "keygen", "--json"])
        .current_dir(dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("cfg")));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let key = dir.path().join("cfg/openxet/token.key");
    assert!(key.is_file());
    assert!(PathBuf::from(format!("{}.pub", key.display())).is_file());
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "wrote into the cwd"
    );
    let out = run(openxet()
        .args(["token", "mint", "--json", "--scope", "s", "--mode", "read"])
        .env("XDG_CONFIG_HOME", dir.path().join("cfg")));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
}

#[test]
fn generate_requires_a_subject() {
    let out = run(openxet().args(["token", "generate", "--json", "--mode", "read"]));
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn concurrent_first_starts_share_one_token() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let spawn = || {
        let data = data.clone();
        thread::spawn(move || Server::start(&data, &[], &[]))
    };
    let (a, b) = (spawn(), spawn());
    let (a, b) = (a.join().unwrap(), b.join().unwrap());
    assert_eq!(a.token(), b.token());
    a.stop();
    b.stop();
}

#[test]
fn a_startup_failure_reports_only_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir");
    std::fs::write(&file, b"").unwrap();
    let out = run(openxet()
        .args(["serve", "--port", "0", "--data-dir"])
        .arg(file.join("data")));
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("connection"), "{stderr}");
    assert!(stderr.contains("error: "), "{stderr}");
}

#[test]
fn the_ready_line_names_the_data_dir_absolutely() {
    let dir = tempfile::tempdir().unwrap();
    let mut cmd = openxet();
    cmd.args(["serve", "--json", "--port", "0", "--data-dir", "rel"])
        .current_dir(dir.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let server = Server::spawn(cmd);
    let shown = PathBuf::from(server.line["data_dir"].as_str().unwrap());
    assert!(shown.is_absolute(), "{shown:?}");
    assert_eq!(
        shown.canonicalize().unwrap(),
        dir.path().join("rel").canonicalize().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn config_check_checks_files_and_the_data_dir() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    std::fs::create_dir(&data).unwrap();
    let key = data.join("transfer.key");
    std::fs::write(&key, format!("{}\n", "a".repeat(64))).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let check = || {
        json_stdout(&run(openxet()
            .args(["config", "check", "--json"])
            .env("OPENXET_SERVER__DATA_DIR", &data)))
    };
    let report = check();
    assert_eq!(report["valid"], false);
    assert_eq!(
        report["problems"][0]["key"], "cas.transfer_key_file",
        "{report}"
    );
    assert!(
        report["problems"][0]["hint"]
            .as_str()
            .unwrap()
            .contains("chmod 600")
    );

    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();
    let report = check();
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(report["valid"], false, "{report}");
    assert_eq!(report["problems"][0]["key"], "server.data_dir", "{report}");
}

#[cfg(unix)]
#[test]
fn a_dangling_data_dir_symlink_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("data");
    std::os::unix::fs::symlink(dir.path().join("gone"), &link).unwrap();

    let report = json_stdout(&run(openxet()
        .args(["config", "check", "--json"])
        .env("OPENXET_SERVER__DATA_DIR", &link)));
    assert_eq!(report["valid"], false, "{report}");
    assert_eq!(report["problems"][0]["key"], "server.data_dir");
    assert!(
        report["problems"][0]["rule"]
            .as_str()
            .unwrap()
            .contains("symbolic link"),
        "{report}"
    );

    let out = run(openxet()
        .args(["serve", "--json", "--port", "0", "--data-dir"])
        .arg(&link));
    assert_eq!(out.status.code(), Some(1));
    let error = serve_error(&out);
    assert_eq!(error["code"], "data_dir");
    assert!(
        error["message"].as_str().unwrap().contains("symbolic link"),
        "{error}"
    );
    assert!(error["hint"].as_str().unwrap().contains("gone"), "{error}");
}

#[test]
fn waiting_for_the_index_lock_is_logged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    let lock = std::fs::File::create(dir.path().join("data/index.sqlite-lock")).unwrap();
    lock.lock().unwrap();

    let mut child = openxet()
        .args(["serve", "--json", "--port", "0", "--data-dir"])
        .arg(dir.path().join("data"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let guard = KillOnDrop(child.id().to_string());
    let stderr = BufReader::new(child.stderr.take().unwrap());
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in stderr.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let next_about_the_lock = || loop {
        let line = rx
            .recv_timeout(PATIENCE)
            .expect("no log about the index lock");
        if line.contains("index.sqlite-lock") {
            break line;
        }
    };
    let waiting = next_about_the_lock();
    assert!(waiting.contains("waiting"), "{waiting}");

    drop(lock);
    let got = next_about_the_lock();
    assert!(got.contains("got the index lock"), "{got}");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.contains("openxet.serve.v1"), "{line}");
    drop(guard);
    let _ = child.wait();
}

/// Kills the process with this pid on drop, so a failing test leaves no
/// server behind.
struct KillOnDrop(String);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = Command::new("kill").args(["-9", &self.0]).status();
    }
}

#[test]
fn a_missing_required_argument_is_named() {
    let out = run(openxet().args(["token", "generate", "--json", "--mode", "read"]));
    assert_eq!(out.status.code(), Some(2));
    let error = json_stderr(&out);
    assert_eq!(error["code"], "usage");
    assert!(
        error["message"].as_str().unwrap().contains("--subject"),
        "{error}"
    );
    assert!(
        !error["message"].as_str().unwrap().contains("Usage:"),
        "{error}"
    );
}

#[test]
fn raw_token_hints_fit_the_key() {
    let token = json_stdout(&run(openxet().args([
        "token",
        "generate",
        "--json",
        "--mode",
        "read",
        "--subject",
        "a",
    ])))["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let report = json_stdout(&run(openxet()
        .args(["config", "check", "--json"])
        .env("OPENXET_SERVER__BIND", &token)));
    let hint = report["problems"][0]["hint"].as_str().unwrap();
    assert!(hint.contains("looks like a raw token"), "{hint}");
    assert!(!hint.contains("SHA-256"), "{hint}");
}

#[cfg(unix)]
#[test]
fn the_shared_dir_warning_names_its_chmod_once() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = json_stdout(&run(openxet()
        .args(["config", "check", "--json"])
        .env("OPENXET_SERVER__DATA_DIR", dir.path())));
    let warning = &report["warnings"][0];
    let text = format!("{} {}", warning["rule"], warning["hint"]);
    assert_eq!(text.matches("chmod 700").count(), 1, "{text}");
}
