//! The composed server (ruling M2-R1): the real `openxet serve` with the
//! CAS, the Hub API and Git smart HTTP on one port, driven by stock `git`
//! (ruling M2-R11) and the Hub routes `huggingface_hub` calls.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead as _, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use common::{Server, noise, openxet, run};
use serde_json::{Value, json};
use tempfile::TempDir;

const PATIENCE: Duration = Duration::from_mins(2);

/// `git args` in `dir`, with no system or user configuration, no prompt,
/// and `env`.
fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let home = dir.join(".git-home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(name);
        }
    }
    cmd.args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env.iter().copied())
        .stdin(Stdio::null());
    run(&mut cmd)
}

/// [`git`], asserting it succeeded; its stderr.
fn must(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
    let out = git(dir, args, env);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "git {args:?}: {stderr}");
    stderr
}

/// `{endpoint}{path}` with `git:{token}@` before the host: git sends the
/// token as the Basic password once the server asks.
fn url(server: &Server, path: &str, token: &str) -> String {
    format!("{}{path}", server.endpoint).replacen("http://", &format!("http://git:{token}@"), 1)
}

/// `{endpoint}{path}`, with no credential in it.
fn plain(server: &Server, path: &str) -> String {
    format!("{}{path}", server.endpoint)
}

/// `env` and `more`.
fn with<'a>(env: &[(&'a str, &'a str)], more: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    env.iter().chain(more).copied().collect()
}

/// A new repository in `dir` with one commit on `main` holding `files`.
fn local_repo(dir: &Path, files: &[(&str, &[u8])]) {
    std::fs::create_dir_all(dir).unwrap();
    must(dir, &["init", "-q", "-b", "main"], &[]);
    commit(dir, "initial", files);
}

fn commit(dir: &Path, message: &str, files: &[(&str, &[u8])]) {
    for (path, content) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    must(dir, &["add", "-A"], &[]);
    must(
        dir,
        &[
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=tester@openxet.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ],
        &[],
    );
}

fn head(dir: &Path) -> String {
    let out = git(dir, &["rev-parse", "HEAD"], &[]);
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A JSON request to the Hub with the `Bearer` credential `token`.
async fn hub(
    server: &Server,
    method: reqwest::Method,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let mut request = reqwest::Client::new()
        .request(method, format!("{}{path}", server.endpoint))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request
            .header("content-type", "application/json")
            .body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

/// `clone_push_fetch_over_http`: stock git pushes (the first push creates
/// the repository), clones over protocol 0 and 2, pushes a large plain
/// blob, and fetches with a gzip request body, through the unprefixed URL,
/// the `.git` URL and a typed prefix. No URL carries a credential: git
/// answers the server's Basic challenge from its credential helper, the
/// token as the password.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one repository's life over every address and protocol, in order"
)]
async fn clone_push_fetch_over_http() {
    let dir = TempDir::new().unwrap();
    let server = Server::start(&dir.path().join("data"), None);
    let token = server.token.clone();
    let src = dir.path().join("src");
    local_repo(&src, &[("README.md", b"# proj\n"), ("src/a.txt", b"a\n")]);

    // No credential in any URL: git sends none until the server's 401
    // offers Basic, then asks its credential helper, here git's own
    // `store` helper holding the token as the password.
    let out = git(
        &src,
        &["push", plain(&server, "/dev/proj").as_str(), "main"],
        &[],
    );
    assert!(
        !out.status.success(),
        "a push without a credential is refused"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("could not read Username"),
        "git was asked for a user name: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let store = dir.path().join("git-credentials");
    std::fs::write(&store, format!("{}\n", url(&server, "", &token))).unwrap();
    let helper_value = format!("store --file={}", store.display());
    let helper = [
        ("GIT_CONFIG_COUNT", "1"),
        ("GIT_CONFIG_KEY_0", "credential.helper"),
        ("GIT_CONFIG_VALUE_0", helper_value.as_str()),
    ];
    let stderr = must(
        &src,
        &["push", plain(&server, "/dev/proj").as_str(), "main"],
        &with(
            &helper,
            &[("GIT_TRACE_CURL", "1"), ("GIT_TRACE_CURL_NO_DATA", "1")],
        ),
    );
    assert!(stderr.contains("[new branch]"), "{stderr}");
    let challenge = stderr.to_ascii_lowercase();
    assert!(
        challenge.contains("401 unauthorized")
            && challenge.contains(r#"www-authenticate: basic realm="openxet""#),
        "the first request went without a credential and got the Basic challenge: {stderr}"
    );
    let (status, info) = hub(
        &server,
        reqwest::Method::GET,
        "/api/models/dev/proj",
        &token,
        None,
    )
    .await;
    assert_eq!(status, 200, "the push created dev/proj: {info}");

    let v0 = dir.path().join("v0");
    let trace = must(
        dir.path(),
        &[
            "-c",
            "protocol.version=0",
            "clone",
            plain(&server, "/dev/proj.git").as_str(),
            v0.to_str().unwrap(),
        ],
        &with(&helper, &[("GIT_TRACE_PACKET", "1")]),
    );
    assert!(!trace.contains("version 2"), "protocol 0 was asked for");
    let v2 = dir.path().join("v2");
    let trace = must(
        dir.path(),
        &[
            "clone",
            plain(&server, "/dev/proj").as_str(),
            v2.to_str().unwrap(),
        ],
        &with(&helper, &[("GIT_TRACE_PACKET", "1")]),
    );
    assert!(trace.contains("version 2"), "protocol 2 is git's default");
    for clone in [&v0, &v2] {
        assert_eq!(head(clone), head(&src));
        assert_eq!(std::fs::read(clone.join("src/a.txt")).unwrap(), b"a\n");
    }

    // A push larger than git's 1 MiB post buffer goes chunked; the blob is
    // plain content, kept as a Git object.
    let large = noise(3 << 20, 7);
    commit(&v2, "large", &[("weights.bin", &large)]);
    must(&v2, &["push", "-q", "origin", "main"], &helper);
    must(&v0, &["pull", "-q", "--ff-only", "origin", "main"], &helper);
    assert_eq!(std::fs::read(v0.join("weights.bin")).unwrap(), large);

    // A fetch whose request is over 1 KiB, here from the haves of 40 local
    // commits, is sent gzipped.
    for i in 0..40 {
        commit(&v0, &format!("local {i}"), &[]);
    }
    commit(&v2, "remote", &[("remote.txt", b"remote\n")]);
    must(&v2, &["push", "-q", "origin", "main"], &helper);
    let trace = must(
        &v0,
        &["fetch", "origin"],
        &with(
            &helper,
            &[("GIT_TRACE_CURL", "1"), ("GIT_TRACE_CURL_NO_DATA", "1")],
        ),
    );
    assert!(
        trace
            .to_ascii_lowercase()
            .contains("content-encoding: gzip"),
        "the fetch request was not gzipped: {trace}"
    );
    assert_eq!(
        String::from_utf8_lossy(&git(&v0, &["rev-parse", "origin/main"], &helper).stdout).trim(),
        head(&v2)
    );

    // A typed prefix: the push creates a dataset, which only that prefix
    // and the unprefixed URL reach.
    must(
        &src,
        &[
            "push",
            "-q",
            plain(&server, "/datasets/dev/data.git").as_str(),
            "main",
        ],
        &helper,
    );
    let (status, _) = hub(
        &server,
        reqwest::Method::GET,
        "/api/datasets/dev/data",
        &token,
        None,
    )
    .await;
    assert_eq!(status, 200);
    for path in ["/datasets/dev/data", "/dev/data.git"] {
        let out = git(
            dir.path(),
            &["ls-remote", url(&server, path, &token).as_str()],
            &[],
        );
        assert!(out.status.success(), "{path}");
        assert!(String::from_utf8_lossy(&out.stdout).contains(&head(&src)));
    }
    let out = git(
        dir.path(),
        &["ls-remote", plain(&server, "/spaces/dev/data").as_str()],
        &helper,
    );
    assert!(!out.status.success(), "dev/data has no space label");

    // A wrong password is refused as such.
    let out = git(
        dir.path(),
        &["ls-remote", url(&server, "/dev/proj", "oxt_wrong").as_str()],
        &[],
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Authentication failed"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Reads the human banner of a server started with `extra` up to its
/// `Stop with` line, then stops it.
fn banner(data_dir: &Path, extra: &[&str]) -> String {
    let mut child = openxet()
        .args(["serve", "--port", "0", "--data-dir"])
        .arg(data_dir)
        .args(extra)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
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
    let banner = rx.recv_timeout(PATIENCE).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();
    banner
}

/// `banner_lists_hub_and_git_lines`: the dev-mode banner prints the
/// `HF_ENDPOINT` and `hf auth login` lines and git commands that run as
/// printed, the first push creating the repository; the configured banner
/// names the Git URL form.
#[tokio::test]
async fn banner_lists_hub_and_git_lines() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("data");
    let text = banner(&data, &[]);
    let token = std::fs::read_to_string(data.join("dev-token"))
        .unwrap()
        .trim()
        .to_owned();
    let endpoint = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("endpoint"))
        .unwrap()
        .trim()
        .to_owned();
    for line in [
        format!("  export HF_ENDPOINT={endpoint}\n"),
        format!("  hf auth login --token {token}\n"),
        format!("  git push {endpoint}/dev/my-model main\n"),
        format!("  git clone {endpoint}/dev/my-model\n"),
    ] {
        assert!(text.contains(&line), "{line:?} missing from:\n{text}");
    }
    assert!(text.contains("password: the token"), "{text}");

    // The printed git commands work as printed, on the same data, with the
    // token as the password.
    let port = endpoint.rsplit(':').next().unwrap();
    let server = Server::start_with(&data, None, port, &[]);
    let src = dir.path().join("src");
    local_repo(&src, &[("model.txt", b"weights\n")]);
    let with_token =
        |command: &str| command.replacen("http://", &format!("http://dev:{token}@"), 1);
    let push = with_token(&format!("{endpoint}/dev/my-model"));
    must(&src, &["push", "-q", push.as_str(), "main"], &[]);
    let clone = with_token(&format!("{endpoint}/dev/my-model"));
    must(dir.path(), &["clone", "-q", clone.as_str()], &[]);
    assert_eq!(
        std::fs::read(dir.path().join("my-model/model.txt")).unwrap(),
        b"weights\n"
    );
    drop(server);

    // Configured mode prints no token, and the URL form.
    let config = dir.path().join("openxet.toml");
    let digest = "0".repeat(64);
    std::fs::write(
        &config,
        format!("[[auth.static_tokens]]\nsha256 = \"{digest}\"\nsubject = \"ci\"\n"),
    )
    .unwrap();
    let text = banner(
        &dir.path().join("configured"),
        &["--config", config.to_str().unwrap()],
    );
    assert!(text.contains("git clone http://127.0.0.1:"), "{text}");
    assert!(text.contains("/<namespace>/<name>"), "{text}");
    assert!(text.contains("the Hub API and Git at"), "{text}");
}

/// `restart_keeps_repos_tokens_and_capabilities`: after a restart on the
/// same data directory, pushed repositories clone, the dev token still
/// works, and a CAS token the Hub handed out before the restart is still
/// accepted by the CAS, as `cas-token.key` is kept.
#[tokio::test]
async fn restart_keeps_repos_tokens_and_capabilities() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("data");
    let server = Server::start(&data, None);
    let token = server.token.clone();
    let src = dir.path().join("src");
    local_repo(&src, &[("a.txt", b"a\n")]);
    must(
        &src,
        &[
            "push",
            "-q",
            url(&server, "/dev/kept", &token).as_str(),
            "main",
        ],
        &[],
    );
    let (status, read) = hub(
        &server,
        reqwest::Method::GET,
        "/api/models/dev/kept/xet-read-token/main",
        &token,
        None,
    )
    .await;
    assert_eq!(status, 200, "{read}");
    let cas_token = read["accessToken"].as_str().unwrap().to_owned();
    let probe = |server: &Server, bearer: String| {
        let url = format!("{}/v1/reconstructions/{}", server.endpoint, "0".repeat(64));
        async move {
            reqwest::Client::new()
                .get(url)
                .bearer_auth(bearer)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(probe(&server, "oxt_not_a_token".to_owned()).await, 401);
    let before = probe(&server, cas_token.clone()).await;
    assert_eq!(before, 404, "the CAS token is accepted: nothing is there");
    let key = std::fs::read(data.join("cas-token.key")).unwrap();
    let port = server.port();
    server.stop();

    let server = Server::start_with(&data, None, &port, &[]);
    assert_eq!(server.token, token, "the dev token changed on restart");
    assert_eq!(std::fs::read(data.join("cas-token.key")).unwrap(), key);
    assert_eq!(probe(&server, cas_token).await, before);
    let clone = dir.path().join("clone");
    must(
        dir.path(),
        &[
            "clone",
            "-q",
            url(&server, "/dev/kept", &token).as_str(),
            clone.to_str().unwrap(),
        ],
        &[],
    );
    assert_eq!(head(&clone), head(&src));
    let (status, _) = hub(
        &server,
        reqwest::Method::GET,
        "/api/models/dev/kept",
        &token,
        None,
    )
    .await;
    assert_eq!(status, 200);
}

/// `credentials_never_logged` (design §5.9 req 6), the Hub and Git half:
/// at `trace` level, through Git smart HTTP with Basic credentials (right
/// and wrong), the Hub routes with `Bearer`, and the CAS with the tokens
/// the Hub hands out, no credential reaches the log, neither as sent nor
/// as the Basic header's base64.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one session of every credential kind, then one assertion over its log"
)]
async fn credentials_never_logged() {
    let dir = TempDir::new().unwrap();
    let server = Server::start_with(
        &dir.path().join("data"),
        None,
        "0",
        &[("OPENXET_SERVER__LOG_LEVEL", "trace")],
    );
    let token = server.token.clone();
    let wrong = format!("{token}x");
    let mut secrets = vec![token.clone(), wrong.clone()];
    for secret in [&token, &wrong] {
        secrets.push(BASE64_STANDARD.encode(format!("git:{secret}")));
    }

    let src = dir.path().join("src");
    local_repo(&src, &[("a.txt", b"a\n")]);
    must(
        &src,
        &[
            "push",
            "-q",
            url(&server, "/dev/logs.git", &token).as_str(),
            "main",
        ],
        &[],
    );
    let clone = dir.path().join("clone");
    must(
        dir.path(),
        &[
            "clone",
            "-q",
            url(&server, "/dev/logs", &token).as_str(),
            clone.to_str().unwrap(),
        ],
        &[],
    );
    commit(&clone, "two", &[("b.txt", b"b\n")]);
    must(&clone, &["push", "-q", "origin", "main"], &[]);
    must(
        &src,
        &["fetch", "-q", url(&server, "/dev/logs", &token).as_str()],
        &[],
    );
    let refused = git(
        dir.path(),
        &["ls-remote", url(&server, "/dev/logs", &wrong).as_str()],
        &[],
    );
    assert!(!refused.status.success());

    let (status, _) = hub(
        &server,
        reqwest::Method::GET,
        "/api/whoami-v2",
        &token,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = hub(
        &server,
        reqwest::Method::POST,
        "/api/repos/create",
        &token,
        Some(json!({"name": "api"})),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = hub(
        &server,
        reqwest::Method::GET,
        "/api/whoami-v2",
        &wrong,
        None,
    )
    .await;
    assert_eq!(status, 401);
    for route in ["xet-read-token", "xet-write-token"] {
        let (status, answer) = hub(
            &server,
            reqwest::Method::GET,
            &format!("/api/models/dev/logs/{route}/main"),
            &token,
            None,
        )
        .await;
        assert_eq!(status, 200, "{answer}");
        let cas = answer["accessToken"].as_str().unwrap().to_owned();
        let status = reqwest::Client::new()
            .get(format!(
                "{}/v1/reconstructions/{}",
                server.endpoint,
                "0".repeat(64)
            ))
            .bearer_auth(&cas)
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(status, 404);
        secrets.push(cas);
    }

    let log = server.stop();
    assert!(
        log.contains("git-receive-pack"),
        "the Git routes were logged: {log}"
    );
    for secret in &secrets {
        assert!(
            !log.contains(secret.as_str()),
            "a credential reached the log"
        );
    }
}

/// The files under `dir` whose path contains `/reflog/`.
fn ref_log_records(dir: &Path) -> usize {
    let mut dirs = vec![dir.to_path_buf()];
    let mut found = 0;
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.to_string_lossy().contains("/reflog/") {
                found += 1;
            }
        }
    }
    found
}

/// `serve` runs the Git engine's maintenance: a push's ref changes reach
/// the bucket ref log within the next pass, well inside its 60 s RPO
/// (design §5.3), without any request asking for it.
#[tokio::test]
async fn maintenance_derives_the_bucket_ref_log() {
    let dir = TempDir::new().unwrap();
    let data = dir.path().join("data");
    let server = Server::start(&data, None);
    let src = dir.path().join("src");
    local_repo(&src, &[("a.txt", b"a\n")]);
    must(
        &src,
        &[
            "push",
            "-q",
            url(&server, "/dev/logged", &server.token).as_str(),
            "main",
        ],
        &[],
    );
    let deadline = std::time::Instant::now() + openxet_git::MAINTENANCE_INTERVAL * 3;
    while ref_log_records(&data.join("bucket")) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "no bucket ref log record within three maintenance intervals"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The data directory stays private with the Git cache in it (design
/// §5.9): after a push and a clone, every directory is `0700` and every
/// file its owner's alone, `0600`, or `0400` for a cached pack file, which
/// is read-only. The `proc-receive` hook links to the openxet binary and
/// holds no data.
#[tokio::test]
async fn data_dir_stays_private_with_git() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = TempDir::new().unwrap();
    let data = dir.path().join("data");
    let server = Server::start(&data, None);
    let src = dir.path().join("src");
    local_repo(&src, &[("a.txt", b"a\n")]);
    let remote = url(&server, "/dev/private", &server.token);
    must(&src, &["push", "-q", remote.as_str(), "main"], &[]);
    must(dir.path(), &["clone", "-q", remote.as_str(), "clone"], &[]);
    server.stop();

    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    let mut dirs = vec![data];
    let mut packs = 0;
    while let Some(dir) = dirs.pop() {
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let kind = std::fs::symlink_metadata(&path).unwrap().file_type();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                dirs.push(path);
                continue;
            }
            let in_cache = path.to_string_lossy().contains("/git-cache/");
            match mode(&path) {
                0o600 => {}
                0o400 if in_cache => packs += 1,
                other => panic!("{} is {other:o}", path.display()),
            }
        }
    }
    assert!(packs >= 2, "the cache holds the pushed pack and its index");
}
