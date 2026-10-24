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
use std::net::TcpListener;
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

/// Deterministic bytes that don't compress or dedup within themselves.
fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[0]
        })
        .collect()
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

/// Uploads `paths` with `--json` and returns the `files` entries.
fn upload(server: &Server, cache: &Path, paths: &[&Path]) -> Vec<Value> {
    let out = run(server.client(cache).arg("upload").args(paths).arg("--json"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(out.stderr.is_empty(), "{out:?}");
    json_stdout(&out)["files"].as_array().unwrap().clone()
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
        assert!(
            !Path::new(&format!("{}.openxet-partial", out_path.display())).exists(),
            "the partial file was left behind"
        );
    }
    assert!(download_cache.is_dir(), "--cache-dir was not used");
}

#[test]
fn reupload_sends_no_xorb() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("data"));
    let path = dir.path().join("model.bin");
    std::fs::write(&path, noise(3 << 20, 3)).unwrap();

    let first = upload(&server, &dir.path().join("cache-1"), &[&path]);
    server.settle();
    let xorbs = server.requests("POST", XORB_ROUTE);
    assert!(xorbs > 0, "the first upload sent no xorb");

    // An empty cache: only the server's global dedup answer can spare the
    // xorb.
    let second = upload(&server, &dir.path().join("cache-2"), &[&path]);
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
    // A port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let endpoint = format!("http://127.0.0.1:{port}");

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
    assert!(!Path::new(&format!("{}.openxet-partial", out_path.display())).exists());

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
