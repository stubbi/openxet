//! `openxet upload` and `openxet download`, run as the real binary against
//! `openxet serve` (`docs/reference/cli.md`).
//!
//! Every client run gets its own Xet cache (`HF_XET_CACHE`), so no test
//! reads or writes the developer's `~/.cache/huggingface`. Requests are
//! counted in the server's own debug log (`server.log_level = "debug"`
//! logs one `started processing request` line per request, with the
//! method and the matched route), so no counter or endpoint exists only
//! for tests.

// The helpers are test code too; a panic is the intended failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// How long any one step may take before the test fails instead of hanging.
const PATIENCE: Duration = Duration::from_mins(2);

/// `openxet` with no inherited openxet or Xet configuration.
fn openxet() -> Command {
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

/// A running dev-mode `openxet serve` with JSON debug logs. Dropping it
/// kills the process.
struct Server {
    child: Child,
    endpoint: String,
    token: String,
    stderr: Arc<Mutex<Vec<u8>>>,
}

impl Server {
    fn start(data_dir: &Path) -> Self {
        Self::start_with(data_dir, &[], "0")
    }

    fn start_with(data_dir: &Path, extra: &[&str], port: &str) -> Self {
        let mut child = openxet()
            .args(["serve", "--json", "--log-format", "json", "--port", port])
            .arg("--data-dir")
            .arg(data_dir)
            .args(extra)
            .env("OPENXET_SERVER__LOG_LEVEL", "debug")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let mut pipe = child.stderr.take().unwrap();
        let sink = Arc::clone(&stderr);
        thread::spawn(move || {
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
            // Keep the pipe open for the server's lifetime.
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
            stderr,
        }
    }

    fn port(&self) -> String {
        self.endpoint.rsplit(':').next().unwrap().to_owned()
    }

    /// The number of requests logged so far with `method` on `route` (a
    /// route template, e.g. `/v1/xorbs/{prefix}/{hash}`).
    fn requests(&self, method: &str, route: &str) -> usize {
        self.log()
            .iter()
            .filter(|line| {
                line["fields"]["message"] == "started processing request"
                    && line["span"]["method"] == method
                    && line["span"]["route"] == route
            })
            .count()
    }

    /// The server's log so far: one JSON object per line.
    fn log(&self) -> Vec<Value> {
        let raw = self.stderr.lock().unwrap().clone();
        String::from_utf8_lossy(&raw)
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// Requests that have been answered, for waiting until the log has
    /// caught up with a finished client run.
    fn answered(&self) -> usize {
        self.log()
            .iter()
            .filter(|line| line["fields"]["message"] == "finished processing request")
            .count()
    }

    fn started(&self) -> usize {
        self.log()
            .iter()
            .filter(|line| line["fields"]["message"] == "started processing request")
            .count()
    }

    /// Waits until every request the server started has been answered and
    /// logged.
    fn settle(&self) {
        let deadline = Instant::now() + PATIENCE;
        while self.answered() < self.started() {
            assert!(Instant::now() < deadline, "the server log never settled");
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// A client command aimed at this server, with its own cache.
    fn client(&self, cache: &Path) -> Command {
        let mut cmd = openxet();
        cmd.env("OPENXET_ENDPOINT", &self.endpoint)
            .env("OPENXET_TOKEN", &self.token)
            .env("HF_XET_CACHE", cache);
        cmd
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn run(cmd: &mut Command) -> Output {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!("the command did not finish: {out:?}");
        }
        thread::sleep(Duration::from_millis(20));
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

/// An endpoint nothing can listen on: no socket is ever bound to port 0
/// (binding 0 picks a free port instead), so connecting fails at once on
/// every platform. A port read from a dropped listener could meanwhile be
/// taken by a concurrent test's `openxet serve --port 0`.
const NEVER_LISTENING: &str = "http://127.0.0.1:0";

fn json_stderr(out: &Output) -> Value {
    let error: Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is not one JSON object ({err}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(error["schema"], "openxet.error.v1", "{error}");
    error
}

/// Deterministic bytes that don't compress or dedup within themselves,
/// distinct for every `seed`: splitmix64 (Steele, Lea and Flood, 2014),
/// whose output is a bijection of its counter, so no two seeds share a
/// stream.
fn noise(len: usize, seed: u64) -> Vec<u8> {
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

#[test]
fn noise_differs_for_every_seed() {
    let streams: Vec<Vec<u8>> = (0..64).map(|seed| noise(64, seed)).collect();
    for (i, a) in streams.iter().enumerate() {
        for (j, b) in streams.iter().enumerate().skip(i + 1) {
            assert_ne!(a, b, "seeds {i} and {j} give the same noise");
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// Uploads `paths` with `--json` and returns the `openxet.upload.v1` object.
fn upload_result(server: &Server, cache: &Path, paths: &[&Path]) -> Value {
    let out = run(server.client(cache).arg("upload").args(paths).arg("--json"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    json_stdout(&out)
}

/// Uploads `paths` with `--json` and returns the `files` entries.
fn upload(server: &Server, cache: &Path, paths: &[&Path]) -> Vec<Value> {
    upload_result(server, cache, paths)["files"]
        .as_array()
        .unwrap()
        .clone()
}

/// Every `*.openxet-partial` file in `dir`.
fn partials(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.to_string_lossy().ends_with(".openxet-partial"))
        .collect()
}

#[cfg(unix)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Sends `signal` (a `kill -s` name) to process `pid`.
#[cfg(unix)]
fn signal(pid: u32, signal: &str) {
    let status = Command::new("kill")
        .args(["-s", signal, &pid.to_string()])
        .status()
        .unwrap();
    assert!(status.success(), "kill -s {signal} {pid} failed");
}

/// Waits until `ready` holds.
fn wait_until(what: &str, ready: impl Fn() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

const XORB_ROUTE: &str = "/v1/xorbs/{prefix}/{hash}";

#[test]
fn upload_then_download_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let cache = dir.path().join("cache");
    let download_cache = dir.path().join("download-cache");
    let contents = [noise(1000, 1), noise(3 << 20, 2), Vec::new()];
    let paths: Vec<PathBuf> = ["small.bin", "large.bin", "empty.bin"]
        .iter()
        .zip(&contents)
        .map(|(name, data)| {
            let path = dir.path().join(name);
            std::fs::write(&path, data).unwrap();
            path
        })
        .collect();

    let files = upload(
        &server,
        &cache,
        &paths.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
    );
    assert_eq!(files.len(), 3);
    for ((file, path), data) in files.iter().zip(&paths).zip(&contents) {
        assert_eq!(file["path"], path.to_str().unwrap());
        assert_eq!(file["size"], data.len());
        assert_eq!(file["sha256"], sha256_hex(data));

        // `--cache-dir` overrides `HF_XET_CACHE`: a fresh cache, so the
        // bytes come from the server.
        let out_path = dir.path().join(format!(
            "{}.out",
            path.file_name().unwrap().to_str().unwrap()
        ));
        let out = run(server
            .client(&cache)
            .args(["download", file["hash"].as_str().unwrap(), "--output"])
            .arg(&out_path)
            .args(["--size", &data.len().to_string(), "--cache-dir"])
            .arg(&download_cache));
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        assert_eq!(&std::fs::read(&out_path).unwrap(), data);
        assert_eq!(partials(dir.path()), Vec::<PathBuf>::new());
    }
    assert!(download_cache.is_dir(), "--cache-dir was not used");
    // The cache lists the user's file and chunk hashes: private.
    #[cfg(unix)]
    {
        assert_eq!(mode_of(&download_cache), 0o700);
        assert_eq!(mode_of(&cache), 0o700);
    }
}

#[test]
fn reupload_sends_no_xorb() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let path = dir.path().join("model.bin");
    std::fs::write(&path, noise(3 << 20, 3)).unwrap();

    let first = upload_result(&server, &dir.path().join("cache-1"), &[&path]);
    assert_eq!(first["bytes_uploaded"], 3 << 20, "{first}");
    assert_eq!(first["bytes_deduplicated"], 0, "{first}");
    let first = first["files"].as_array().unwrap().clone();
    server.settle();
    let xorbs = server.requests("POST", XORB_ROUTE);
    assert!(xorbs > 0, "the first upload sent no xorb");

    // An empty cache: only the server's global dedup answer can spare the
    // xorb.
    let second = upload_result(&server, &dir.path().join("cache-2"), &[&path]);
    assert_eq!(second["bytes_uploaded"], 0, "{second}");
    assert_eq!(second["bytes_deduplicated"], 3 << 20, "{second}");
    let second = second["files"].as_array().unwrap().clone();
    server.settle();
    assert_eq!(second[0]["hash"], first[0]["hash"]);
    assert_eq!(
        server.requests("POST", XORB_ROUTE),
        xorbs,
        "the second upload sent a xorb"
    );
    assert!(
        server.requests("GET", "/v1/chunks/{prefix}/{hash}") > 0,
        "the second upload never asked for global dedup"
    );
}

/// The stored bytes of the xorbs the server accepted after the first
/// `since` lines of its log: what clients sent, plus each stored footer,
/// so never less than the bodies on the wire.
fn xorb_bytes_accepted(server: &Server, data_dir: &Path, since: usize) -> u64 {
    let hashes: Vec<String> = server.log()[since..]
        .iter()
        .filter(|line| line["fields"]["message"] == "xorb upload accepted")
        .map(|line| line["fields"]["xorb"].as_str().unwrap().to_owned())
        .collect();
    let mut sizes = std::collections::HashMap::new();
    let mut dirs = vec![data_dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.to_string_lossy().contains("xorbs") {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                sizes.insert(name, std::fs::metadata(&path).unwrap().len());
            }
        }
    }
    hashes
        .iter()
        .map(|h| {
            sizes
                .get(h)
                .copied()
                .unwrap_or_else(|| panic!("xorb {h} not stored"))
        })
        .sum()
}

#[test]
fn fresh_cache_edit_dedups_against_the_server() {
    // Design §1a: after a 1 MiB edit in the middle of a 100 MiB file, a
    // client with an empty cache sends at most 2 MiB of xorbs, because
    // one dedup answer offers every xorb of the first upload's shard
    // (ruling M1-R30, S11). Before it, about 30 MB went.
    const SIZE: usize = 100 << 20;
    const EDIT: usize = 1 << 20;
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let server = Server::start(&data);
    let original = noise(SIZE, 30);
    let mut edited = original.clone();
    edited[SIZE / 2..SIZE / 2 + EDIT].copy_from_slice(&noise(EDIT, 32));
    let (a, b) = (dir.path().join("a.bin"), dir.path().join("b.bin"));
    std::fs::write(&a, &original).unwrap();
    std::fs::write(&b, &edited).unwrap();
    drop((original, edited));

    upload_result(&server, &dir.path().join("cache-1"), &[&a]);
    server.settle();
    let mark = server.log().len();
    let dedup_queries = server.requests("GET", "/v1/chunks/{prefix}/{hash}");

    let second = upload_result(&server, &dir.path().join("cache-2"), &[&b]);
    server.settle();
    let sent = xorb_bytes_accepted(&server, &data, mark);
    assert!(
        sent <= 2 << 20,
        "a 1 MiB edit sent {sent} bytes of xorbs with a fresh cache: {second}"
    );
    assert!(
        server.requests("GET", "/v1/chunks/{prefix}/{hash}") > dedup_queries,
        "the second upload never asked for global dedup"
    );
    assert!(
        second["bytes_deduplicated"].as_u64().unwrap() >= (SIZE - 2 * EDIT) as u64,
        "{second}"
    );
}

#[test]
fn no_telemetry_sent() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let path = dir.path().join("a.bin");
    std::fs::write(&path, noise(1 << 20, 4)).unwrap();
    let cache = dir.path().join("cache");

    // The client must not send telemetry even where the environment asks
    // xet-core to (non-negotiable 6: only the operator opts in).
    let out = run(server
        .client(&cache)
        .env("HF_XET_TELEMETRY_ENABLED", "true")
        .arg("upload")
        .arg(&path)
        .arg("--json"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let hash = json_stdout(&out)["files"][0]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let out = run(server
        .client(&cache)
        .env("HF_XET_TELEMETRY_ENABLED", "true")
        .args(["download", &hash, "--json", "--output"])
        .arg(dir.path().join("a.out")));
    assert_eq!(out.status.code(), Some(0), "{out:?}");

    server.settle();
    assert!(
        server.requests("POST", XORB_ROUTE) > 0,
        "the request log saw no upload"
    );
    let telemetry = server
        .log()
        .iter()
        .filter(|line| {
            line["span"]["route"]
                .as_str()
                .is_some_and(|route| route.contains("telemetry"))
        })
        .count();
    assert_eq!(telemetry, 0, "the client sent telemetry");
}

#[test]
fn json_output_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let cache = dir.path().join("cache");
    let path = dir.path().join("a.bin");
    let data = noise(4096, 5);
    std::fs::write(&path, &data).unwrap();

    let out = run(server.client(&cache).arg("upload").arg(&path).arg("--json"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    let upload = json_stdout(&out);
    assert_eq!(upload["schema"], "openxet.upload.v1");
    let mut top: Vec<&str> = upload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    top.sort_unstable();
    assert_eq!(
        top,
        ["bytes_deduplicated", "bytes_uploaded", "files", "schema"],
        "{upload}"
    );
    assert_eq!(
        upload["bytes_uploaded"].as_u64().unwrap() + upload["bytes_deduplicated"].as_u64().unwrap(),
        4096,
        "{upload}"
    );
    let file = &upload["files"][0];
    let keys: Vec<&str> = file
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys.len(), 4, "{file}");
    for key in ["path", "hash", "size", "sha256"] {
        assert!(keys.contains(&key), "{key} missing from {file}");
    }
    let hash = file["hash"].as_str().unwrap();
    assert_eq!(hash.len(), 64);
    assert!(
        hash.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    assert_eq!(file["size"], 4096);

    let out_path = dir.path().join("a.out");
    let out = run(server
        .client(&cache)
        .args(["download", hash, "--json", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    let download = json_stdout(&out);
    assert_eq!(download["schema"], "openxet.download.v1");
    assert_eq!(download["hash"], hash);
    assert_eq!(download["path"], out_path.to_str().unwrap());
    assert_eq!(download["size"], 4096);
    assert_eq!(download.as_object().unwrap().len(), 4, "{download}");

    // Human output: a table naming each file, its hash, size and sha256.
    let out = run(server.client(&cache).arg("upload").arg(&path));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(hash), "{text}");
    assert!(text.contains(&sha256_hex(&data)), "{text}");
    assert!(text.contains(path.to_str().unwrap()), "{text}");
    // The second upload of the same bytes is all dedup, and says so.
    assert!(
        text.contains("Uploaded 0 B of 4.0 KiB (100% deduplicated)"),
        "{text}"
    );
    let out = run(server
        .client(&cache)
        .args(["download", hash, "--force", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("(4.0 KiB)"), "{text}");
    // stderr is not a terminal here: no progress.
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn download_refuses_to_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let cache = dir.path().join("cache");
    let path = dir.path().join("a.bin");
    std::fs::write(&path, b"new bytes").unwrap();
    let files = upload(&server, &cache, &[&path]);
    let hash = files[0]["hash"].as_str().unwrap();
    let out_path = dir.path().join("existing");
    std::fs::write(&out_path, b"keep me").unwrap();

    let out = run(server
        .client(&cache)
        .args(["download", hash, "--json", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let error = json_stderr(&out);
    assert_eq!(error["code"], "output_exists");
    assert!(
        error["hint"].as_str().unwrap().contains("--force"),
        "{error}"
    );
    assert_eq!(std::fs::read(&out_path).unwrap(), b"keep me");

    let out = run(server
        .client(&cache)
        .args(["download", hash, "--json", "--force", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(std::fs::read(&out_path).unwrap(), b"new bytes");
}

#[test]
fn missing_token_is_actionable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.bin");
    std::fs::write(&path, b"x").unwrap();
    let out = run(openxet()
        .env("OPENXET_ENDPOINT", "http://127.0.0.1:1")
        .env("HF_XET_CACHE", dir.path().join("cache"))
        .arg("upload")
        .arg(&path)
        .arg("--json"));
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let error = json_stderr(&out);
    assert_eq!(error["code"], "missing_token");
    assert!(
        error["hint"].as_str().unwrap().contains("OPENXET_TOKEN"),
        "{error}"
    );

    let out = run(openxet()
        .env("HF_XET_CACHE", dir.path().join("cache"))
        .arg("upload")
        .arg(&path));
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let text = String::from_utf8(out.stderr).unwrap();
    assert!(text.contains("OPENXET_ENDPOINT"), "{text}");
}

#[test]
fn unreachable_endpoint_is_actionable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.bin");
    std::fs::write(&path, b"x").unwrap();
    let endpoint = NEVER_LISTENING.to_owned();

    let started = Instant::now();
    let out = run(openxet()
        .env("OPENXET_ENDPOINT", &endpoint)
        .env("OPENXET_TOKEN", "oxt_unused")
        .env("HF_XET_CACHE", dir.path().join("cache"))
        .arg("upload")
        .arg(&path)
        .arg("--json"));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "an unreachable endpoint took {:?} to report",
        started.elapsed()
    );
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let error = json_stderr(&out);
    assert_eq!(error["code"], "unreachable_endpoint");
    assert!(
        error["message"].as_str().unwrap().contains(&endpoint),
        "{error}"
    );
    assert!(
        error["hint"].as_str().unwrap().contains("OPENXET_ENDPOINT"),
        "{error}"
    );
}

#[test]
fn a_missing_path_uploads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let present = dir.path().join("present.bin");
    std::fs::write(&present, noise(1000, 6)).unwrap();
    let missing = dir.path().join("missing.bin");

    let out = run(server
        .client(&dir.path().join("cache"))
        .arg("upload")
        .arg(&present)
        .arg(&missing)
        .arg(dir.path())
        .arg("--json"));
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let error = json_stderr(&out);
    assert_eq!(error["code"], "invalid_input");
    let files = error["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{error}");
    assert_eq!(files[0]["path"], missing.to_str().unwrap());
    assert_eq!(files[0]["problem"], "not_found");
    assert_eq!(files[1]["path"], dir.path().to_str().unwrap());
    assert_eq!(files[1]["problem"], "not_a_file");
    server.settle();
    assert_eq!(server.started(), 0, "a request was sent");
}

#[test]
fn unknown_hash_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let out_path = dir.path().join("out");
    let out = run(server
        .client(&dir.path().join("cache"))
        .args(["download", &"ab".repeat(32), "--json", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert_eq!(json_stderr(&out)["code"], "not_found");
    assert!(!out_path.exists());
    assert_eq!(partials(dir.path()), Vec::<PathBuf>::new());

    let out = run(server
        .client(&dir.path().join("cache"))
        .args(["download", "not-a-hash", "--json", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert_eq!(json_stderr(&out)["code"], "invalid_hash");
}

/// Static tokens for a configured server: a write and a read token, and
/// the config entries that trust them.
fn static_tokens(dir: &Path) -> (String, String, PathBuf) {
    let generate = |mode: &str| {
        json_stdout(&run(openxet().args([
            "token",
            "generate",
            "--json",
            "--mode",
            mode,
            "--subject",
            mode,
        ])))
    };
    let (write, read) = (generate("write"), generate("read"));
    let config = dir.join("openxet.toml");
    std::fs::write(
        &config,
        format!(
            "{}\n{}\n",
            write["config"].as_str().unwrap(),
            read["config"].as_str().unwrap()
        ),
    )
    .unwrap();
    (
        write["token"].as_str().unwrap().to_owned(),
        read["token"].as_str().unwrap().to_owned(),
        config,
    )
}

#[test]
fn refused_credentials_are_actionable_and_never_printed() {
    let dir = tempfile::tempdir().unwrap();
    let (write, read, config) = static_tokens(dir.path());
    let server = Server::start_with(
        &dir.path().join("data"),
        &["--config", config.to_str().unwrap()],
        "0",
    );
    let path = dir.path().join("a.bin");
    std::fs::write(&path, noise(1000, 7)).unwrap();
    let wrong = format!("{write}x");

    let attempt = |token: &str, json: bool| {
        let mut cmd = server.client(&dir.path().join("cache"));
        cmd.env("OPENXET_TOKEN", token).arg("upload").arg(&path);
        if json {
            cmd.arg("--json");
        }
        run(&mut cmd)
    };
    for json in [true, false] {
        for (token, code) in [(&read, "forbidden"), (&wrong, "unauthorized")] {
            let out = attempt(token, json);
            assert_eq!(out.status.code(), Some(1), "{out:?}");
            let printed = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(!printed.contains(token.as_str()), "the token was printed");
            if json {
                assert_eq!(json_stderr(&out)["code"], code);
            } else {
                assert!(printed.contains("hint:"), "{printed}");
            }
        }
    }
    let out = attempt(&write, true);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains(write.as_str()));
}

#[test]
fn a_reset_server_rejection_is_passed_through() {
    // The client's shard cache still lists a xorb that a reset server no
    // longer has: the server's message, with its stale-cache note, is what
    // the user needs (S10 "Our client core", ruling M1-R5).
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("cache");
    let path = dir.path().join("a.bin");
    std::fs::write(&path, noise(1 << 20, 8)).unwrap();
    let port = {
        let server = Server::start(&dir.path().join("data-1"));
        upload(&server, &cache, &[&path]);
        server.port()
    };
    let server = Server::start_with(&dir.path().join("data-2"), &[], &port);
    // The same bytes and a different name: the client dedups them all
    // against its cached shard.
    let again = dir.path().join("b.bin");
    std::fs::copy(&path, &again).unwrap();

    let out = run(server
        .client(&cache)
        .arg("upload")
        .arg(&again)
        .arg("--json"));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let error = json_stderr(&out);
    assert_eq!(error["code"], "upload_rejected");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("clear the client's Xet cache"), "{error}");
}

#[test]
fn an_expired_jwt_fails_before_any_request() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let path = dir.path().join("a.bin");
    std::fs::write(&path, b"x").unwrap();
    // Expired long ago. The client reads `exp` without verifying; the
    // server verifies. Built at run time, so no JWT-shaped literal is
    // committed (the secret scanner flags them).
    let segment = |json: &str| URL_SAFE_NO_PAD.encode(json);
    let jwt = format!(
        "{}.{}.{}",
        segment(r#"{"alg":"EdDSA"}"#),
        segment(r#"{"exp":1}"#),
        segment("sig")
    );
    let jwt = jwt.as_str();

    let out = run(server
        .client(&dir.path().join("cache"))
        .env("OPENXET_TOKEN", jwt)
        .arg("upload")
        .arg(&path)
        .arg("--json"));
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let error = json_stderr(&out);
    assert_eq!(error["code"], "token_expired");
    assert!(
        error["hint"].as_str().unwrap().contains("token mint"),
        "{error}"
    );
    assert!(!String::from_utf8_lossy(&out.stderr).contains(jwt));
    server.settle();
    assert_eq!(server.started(), 0, "a request was sent");
}

#[test]
#[cfg(unix)]
fn a_symlink_at_a_partial_path_is_never_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let cache = dir.path().join("cache");
    let path = dir.path().join("a.bin");
    let data = noise(1000, 9);
    std::fs::write(&path, &data).unwrap();
    let hash = upload(&server, &cache, &[&path])[0]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let victim = dir.path().join("victim");
    std::fs::write(&victim, b"untouched").unwrap();
    let out_path = dir.path().join("out");
    // Every name a partial file could predictably have.
    for planted in ["out.openxet-partial", ".out.openxet-partial"] {
        std::os::unix::fs::symlink(&victim, dir.path().join(planted)).unwrap();
    }

    let out = run(server
        .client(&cache)
        .args(["download", &hash, "--json", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
    assert!(
        !std::fs::symlink_metadata(&out_path)
            .unwrap()
            .file_type()
            .is_symlink(),
        "--output became a symlink"
    );
    assert_eq!(std::fs::read(&out_path).unwrap(), data);
    // Only the planted links remain.
    assert_eq!(partials(dir.path()).len(), 2);
}

/// Interrupts a download stalled against a stopped server once its
/// partial file exists, and checks it is cleaned up.
#[cfg(unix)]
fn interrupted_download(sig: &str) {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let cache = dir.path().join("cache");
    let path = dir.path().join("a.bin");
    std::fs::write(&path, noise(1000, 10)).unwrap();
    let hash = upload(&server, &cache, &[&path])[0]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();
    // A stopped server still accepts connections into its backlog, so the
    // client stalls on its first request.
    signal(server.child.id(), "STOP");

    let child = server
        .client(&cache)
        .args(["download", &hash, "--json", "--output"])
        .arg(out_dir.join("a.out"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_until("the partial file", || !partials(&out_dir).is_empty());
    signal(child.id(), sig);
    let out = child.wait_with_output().unwrap();
    signal(server.child.id(), "CONT");

    assert_eq!(out.status.code(), Some(130), "{out:?}");
    let error = json_stderr(&out);
    assert_eq!(error["code"], "interrupted");
    let target = out_dir.join("a.out");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .ends_with(&format!("nothing was written to {}", target.display())),
        "{error}"
    );
    assert!(out.stdout.is_empty(), "{out:?}");
    assert_eq!(partials(&out_dir), Vec::<PathBuf>::new());
    assert!(!out_dir.join("a.out").exists());
}

#[test]
#[cfg(unix)]
fn sigint_during_a_download_removes_the_partial_file() {
    interrupted_download("INT");
}

#[test]
#[cfg(unix)]
fn sigterm_during_a_download_removes_the_partial_file() {
    interrupted_download("TERM");
}

#[test]
#[cfg(unix)]
fn an_interrupted_upload_exits_130() {
    for sig in ["INT", "TERM"] {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(&dir.path().join("data"));
        let cache = dir.path().join("cache");
        let path = dir.path().join("a.bin");
        std::fs::write(&path, noise(1000, 11)).unwrap();
        signal(server.child.id(), "STOP");

        let child = server
            .client(&cache)
            .arg("upload")
            .arg(&path)
            .arg("--json")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // The cache appears once the session starts.
        wait_until("the client cache", || cache.is_dir());
        signal(child.id(), sig);
        let out = child.wait_with_output().unwrap();
        signal(server.child.id(), "CONT");

        assert_eq!(out.status.code(), Some(130), "{sig}: {out:?}");
        let error = json_stderr(&out);
        assert_eq!(error["code"], "interrupted", "{sig}");
        // A signal during finalize may land after the server took the
        // shard: the message must not claim nothing was registered.
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("the upload may or may not have been registered"),
            "{error}"
        );
        assert!(
            error["hint"].as_str().unwrap().contains("idempotent"),
            "{error}"
        );
        assert!(out.stdout.is_empty(), "{out:?}");
    }
}

#[test]
fn a_wrong_size_is_size_mismatch() {
    // Run in a debug build too: xet-data's debug assertions must never see
    // a wrong size.
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let cache = dir.path().join("cache");
    let path = dir.path().join("a.bin");
    std::fs::write(&path, noise(1000, 12)).unwrap();
    let hash = upload(&server, &cache, &[&path])[0]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let out_dir = dir.path().join("out");
    std::fs::create_dir(&out_dir).unwrap();

    for size in ["999", "1001"] {
        let out = run(server
            .client(&cache)
            .args(["download", &hash, "--json", "--size", size, "--output"])
            .arg(out_dir.join("a.out")));
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let error = json_stderr(&out);
        assert_eq!(error["code"], "size_mismatch");
        assert!(
            error["hint"].as_str().unwrap().contains("--size"),
            "{error}"
        );
        assert!(
            error["message"].as_str().unwrap().contains("1000"),
            "{error}"
        );
        assert_eq!(std::fs::read_dir(&out_dir).unwrap().count(), 0);
    }
}

#[test]
fn no_proxy_is_honoured_and_a_proxy_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.bin");
    std::fs::write(&path, b"x").unwrap();
    let endpoint = NEVER_LISTENING.to_owned();
    let proxy = "http://localhost:0".to_owned();
    let attempt = |no_proxy: &str| {
        let started = Instant::now();
        let out = run(openxet()
            .env("OPENXET_ENDPOINT", &endpoint)
            .env("OPENXET_TOKEN", "oxt_unused")
            .env("HF_XET_CACHE", dir.path().join("cache"))
            .env("HTTP_PROXY", &proxy)
            .env("NO_PROXY", no_proxy)
            .arg("upload")
            .arg(&path)
            .arg("--json"));
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "took {:?}",
            started.elapsed()
        );
        assert_eq!(out.status.code(), Some(1), "{out:?}");
        let error = json_stderr(&out);
        assert_eq!(error["code"], "unreachable_endpoint");
        error["message"].as_str().unwrap().to_owned()
    };

    // The endpoint bypasses the proxy: it is dialled itself.
    let message = attempt("127.0.0.1");
    assert!(message.contains(&endpoint), "{message}");
    assert!(!message.contains("proxy"), "{message}");
    // Through the proxy: the proxy is what is unreachable.
    let message = attempt("");
    assert!(message.contains(&proxy), "{message}");
    assert!(message.contains("proxy"), "{message}");
}

#[test]
fn download_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let cache = dir.path().join("cache");
    let path = dir.path().join("a.bin");
    let data = noise(1000, 13);
    std::fs::write(&path, &data).unwrap();
    let hash = upload(&server, &cache, &[&path])[0]["hash"]
        .as_str()
        .unwrap()
        .to_owned();
    let out_path = dir.path().join("models/v1/a.bin");

    let out = run(server
        .client(&cache)
        .args(["download", &hash, "--json", "--output"])
        .arg(&out_path));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(std::fs::read(&out_path).unwrap(), data);
    // User output directories get the usual mode (the umask's), not the
    // cache's 0700: compare with one this process creates.
    #[cfg(unix)]
    {
        let reference = dir.path().join("reference");
        std::fs::create_dir(&reference).unwrap();
        assert_eq!(mode_of(&dir.path().join("models")), mode_of(&reference));
        assert_eq!(mode_of(&dir.path().join("models/v1")), mode_of(&reference));
    }
}
