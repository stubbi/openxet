//! The push harness: engines whose `proc-receive` hook is the test hook,
//! a smart-HTTP server in front of them that real `git push` talks to, and
//! client repositories. Fetches read a snapshot directory directly: R22's
//! interposer is Task 7's, and these tests are about writes.

#![allow(dead_code, clippy::missing_panics_doc)]

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures_util::StreamExt as _;
use http_body_util::{BodyExt, Full, StreamBody, combinators::UnsyncBoxBody};
use hyper::body::{Frame, Incoming};
use hyper::{Request, Response, StatusCode};
use openxet_auth::UserPrincipal;
use openxet_git::{Engine, GitConfig, git_command};
use openxet_index::{GitStore as _, Oid, RepoId};
use tempfile::TempDir;

use crate::common::{World, fixture, fixture_repo, ok, pack_objects, rev, set};

/// The test hook binary.
pub const TEST_HOOK: &str = env!("CARGO_BIN_EXE_openxet-git-test-hook");

/// A config for an engine whose cache is `{world}/{name}`, running the test
/// hook.
pub fn config(world: &World, name: &str) -> GitConfig {
    let mut config = GitConfig::new(world.dir.path().join(name));
    config.hook_program = Some(PathBuf::from(TEST_HOOK));
    config
}

pub async fn engine(world: &World, config: GitConfig) -> Arc<Engine> {
    Arc::new(ok(Engine::start(
        world.index.clone(),
        world.bucket.clone(),
        config,
    )
    .await))
}

pub fn principal(subject: &str) -> UserPrincipal {
    UserPrincipal {
        subject: subject.to_owned(),
        name: subject.to_owned(),
        email: None,
        claims: BTreeMap::new(),
    }
}

/// A repository seeded from a `tf10k` fixture of `commits` commits: `main`
/// and the tag `v1.0` committed in one pack. Returns the repository and the
/// fixture.
pub async fn seeded(world: &World, commits: u32) -> (RepoId, TempDir) {
    let fx = fixture("tf10k", commits);
    let src = fixture_repo(&fx);
    let repo = world.repo("org", &format!("r{}", uuid_like())).await;
    let out = world.dir.path().join(format!("seed-{repo}"));
    let pack = pack_objects(&src, "refs/heads/main\nrefs/tags/v1.0", &out);
    world
        .commit(
            repo,
            &pack,
            1_761_400_000,
            &[
                set("refs/heads/main", None, rev(&src, "refs/heads/main")),
                set("refs/tags/v1.0", None, rev(&src, "refs/tags/v1.0")),
            ],
        )
        .await;
    (repo, fx)
}

fn uuid_like() -> String {
    RepoId::generate().to_string()[20..].to_owned()
}

/// The ref's target in the index.
pub async fn db_ref(world: &World, repo: RepoId, name: &str) -> Option<Oid> {
    ok(world.index.snapshot(repo).await)
        .refs
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, o)| o)
}

/// What one `git` run printed.
#[derive(Debug)]
pub struct Run {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Runs a client `git` without the caller's configuration, never panicking.
pub fn client_git(dir: &Path, args: &[&str]) -> Run {
    let out: Output = ok(Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=Pusher",
            "-c",
            "user.email=pusher@openxet.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "protocol.version=0",
        ])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_DATE", "1761400000 +0000")
        .env("GIT_COMMITTER_DATE", "1761400000 +0000")
        .stdin(Stdio::null())
        .output());
    Run {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// [`client_git`] that must succeed; returns stdout.
pub fn must(dir: &Path, args: &[&str]) -> String {
    let run = client_git(dir, args);
    assert!(run.success, "git {args:?} failed: {}", run.stderr);
    run.stdout
}

/// A non-bare clone of `source` (a path) in a new temp dir, with `origin`
/// pointing at `url`.
pub fn clone(source: &Path, url: &str, extra: &[&str]) -> TempDir {
    let dir = ok(TempDir::new());
    let mut args = vec!["clone", "-q"];
    args.extend_from_slice(extra);
    let src = source.to_string_lossy().into_owned();
    args.extend(["--", &src, "work"]);
    must(dir.path(), &args);
    let work = dir.path().join("work");
    must(&work, &["remote", "set-url", "origin", url]);
    dir
}

pub fn work(dir: &TempDir) -> PathBuf {
    dir.path().join("work")
}

/// Commits `files` (path, contents) in `work`, returning the commit.
pub fn commit(work: &Path, message: &str, files: &[(&str, &[u8])]) -> Oid {
    for (path, contents) in files {
        let file = work.join(path);
        if let Some(parent) = file.parent() {
            ok(std::fs::create_dir_all(parent));
        }
        ok(std::fs::write(file, contents));
        must(work, &["add", "--", path]);
    }
    must(work, &["commit", "-q", "--allow-empty", "-m", message]);
    ok(must(work, &["rev-parse", "HEAD"]).trim().parse())
}

/// Fetches `repo`'s refs from a fresh snapshot of `engine` into
/// `refs/remotes/origin/*` of `work`.
pub async fn fetch(engine: &Engine, repo: RepoId, work: &Path) {
    let snap = ok(engine.snapshot(repo).await);
    let dir = snap.dir.path().to_string_lossy().into_owned();
    must(
        work,
        &["fetch", "-q", &dir, "+refs/heads/*:refs/remotes/origin/*"],
    );
    drop(snap);
}

/// `git fsck --strict` of a fresh snapshot of `repo` on `engine`.
pub async fn fsck(engine: &Engine, repo: RepoId) {
    let snap = ok(engine.snapshot(repo).await);
    let out = ok(git_command(engine.git(), &snap, None)
        .args(["fsck", "--strict", "--no-dangling"])
        .output()
        .await);
    assert!(
        out.status.success(),
        "fsck --strict failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

type Body = UnsyncBoxBody<Bytes, std::io::Error>;

struct State {
    engines: Vec<Arc<Engine>>,
    who: Mutex<UserPrincipal>,
    /// An advertisement served instead of a fresh one: a client's view
    /// older than the snapshot its push runs on.
    frozen: Mutex<Option<Vec<u8>>>,
}

/// A smart-HTTP server for `git push` in front of `engines`: the push URL of
/// repository `repo` on engine `i` is [`Server::url`].
pub struct Server {
    addr: std::net::SocketAddr,
    state: Arc<State>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Server {
    pub async fn start(engines: Vec<Arc<Engine>>) -> Self {
        let listener = ok(tokio::net::TcpListener::bind("127.0.0.1:0").await);
        let addr = ok(listener.local_addr());
        let state = Arc::new(State {
            engines,
            who: Mutex::new(principal("pusher")),
            frozen: Mutex::new(None),
        });
        let shared = Arc::clone(&state);
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let state = Arc::clone(&shared);
                tokio::spawn(async move {
                    let service = hyper::service::service_fn(move |req| {
                        let state = Arc::clone(&state);
                        async move { Ok::<_, Infallible>(serve(&state, req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(hyper_util::rt::TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self { addr, state, task }
    }

    pub fn url(&self, engine: usize, repo: RepoId) -> String {
        format!("http://{}/{engine}/{repo}.git", self.addr)
    }

    /// Serves `repo`'s current advertisement on `engine` to every push until
    /// [`Server::thaw_advertisement`].
    pub async fn freeze_advertisement(&self, engine: usize, repo: RepoId) {
        let ad = advertisement(&self.state.engines[engine], repo).await;
        *lock(&self.state.frozen) = Some(ad);
    }

    pub fn thaw_advertisement(&self) {
        *lock(&self.state.frozen) = None;
    }

    /// Pushes from now on act as `subject`.
    pub fn act_as(&self, subject: &str) {
        *lock(&self.state.who) = principal(subject);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

async fn advertisement(engine: &Engine, repo: RepoId) -> Vec<u8> {
    let snap = ok(engine.snapshot(repo).await);
    let out = ok(git_command(engine.git(), &snap, None)
        .args(["receive-pack", "--advertise-refs", "--stateless-rpc", "."])
        .output()
        .await);
    let mut body = b"001f# service=git-receive-pack\n0000".to_vec();
    body.extend_from_slice(&out.stdout);
    body
}

fn plain(status: StatusCode, text: &str) -> Response<Body> {
    let mut response = Response::new(
        Full::new(Bytes::from(text.to_owned()))
            .map_err(|never| match never {})
            .boxed_unsync(),
    );
    *response.status_mut() = status;
    response
}

async fn serve(state: &State, request: Request<Incoming>) -> Response<Body> {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or_default().to_owned();
    let mut parts = path.trim_start_matches('/').splitn(2, '/');
    let (Some(engine), Some(rest)) = (parts.next(), parts.next()) else {
        return plain(StatusCode::NOT_FOUND, "no such path");
    };
    let Some(engine) = engine
        .parse::<usize>()
        .ok()
        .and_then(|i| state.engines.get(i))
    else {
        return plain(StatusCode::NOT_FOUND, "no such engine");
    };
    let Some((repo, action)) = rest.split_once(".git/") else {
        return plain(StatusCode::NOT_FOUND, "no such path");
    };
    let Ok(repo) = repo.parse::<RepoId>() else {
        return plain(StatusCode::NOT_FOUND, "no such repository");
    };
    match action {
        "info/refs" if query == "service=git-receive-pack" => {
            let frozen = lock(&state.frozen).clone();
            let body = match frozen {
                Some(ad) => ad,
                None => advertisement(engine, repo).await,
            };
            let mut response = plain(StatusCode::OK, "");
            *response.body_mut() = Full::new(Bytes::from(body))
                .map_err(|never| match never {})
                .boxed_unsync();
            response.headers_mut().insert(
                "content-type",
                hyper::header::HeaderValue::from_static(
                    "application/x-git-receive-pack-advertisement",
                ),
            );
            response
        }
        "git-receive-pack" => {
            let who = lock(&state.who).clone();
            let body = request
                .into_body()
                .into_data_stream()
                .map(|r| r.map_err(std::io::Error::other));
            let body: openxet_storage::BodyStream = Box::pin(body);
            match engine.receive_pack(repo, 0, &who, body).await {
                Ok(stream) => {
                    let frames = stream.map(|r| r.map(Frame::data));
                    let mut response =
                        Response::new(BodyExt::boxed_unsync(StreamBody::new(frames)));
                    response.headers_mut().insert(
                        "content-type",
                        hyper::header::HeaderValue::from_static(
                            "application/x-git-receive-pack-result",
                        ),
                    );
                    response
                }
                Err(e) => plain(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            }
        }
        _ => plain(StatusCode::NOT_FOUND, "not served by the test server"),
    }
}

/// What receive-pack answered a hand-made request: the report-status
/// lines and the side-band messages (`remote: …` text).
#[derive(Debug)]
pub struct RawResult {
    pub report: Vec<String>,
    pub messages: String,
}

impl RawResult {
    /// The `ng` reason for `name`, if it was refused.
    pub fn ng(&self, name: &str) -> Option<String> {
        let prefix = format!("ng {name} ");
        self.report
            .iter()
            .find_map(|l| l.strip_prefix(&prefix).map(str::to_owned))
    }

    pub fn ok(&self, name: &str) -> bool {
        self.report.iter().any(|l| *l == format!("ok {name}"))
    }
}

fn pkt(payload: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", payload.len() + 4).into_bytes();
    out.extend_from_slice(payload);
    out
}

/// Splits a pkt-line stream into payloads, skipping flushes.
fn pkt_payloads(mut data: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let n =
            usize::from_str_radix(std::str::from_utf8(&data[..4]).unwrap_or("0"), 16).unwrap_or(0);
        if n < 4 {
            data = &data[4..];
            continue;
        }
        out.push(data[4..n.min(data.len())].to_vec());
        data = &data[n.min(data.len())..];
    }
    out
}

/// Sends a hand-made receive-pack request to `engine`: `commands` as
/// `(old, new, ref)`, then `pack` when given, with side-band so the hook's
/// messages come back.
pub async fn raw_push(
    engine: &Engine,
    repo: RepoId,
    who: &str,
    commands: &[(Option<Oid>, Option<Oid>, &str)],
    pack: Option<Vec<u8>>,
) -> RawResult {
    let zero = "0".repeat(40);
    let mut req = Vec::new();
    for (i, (old, new, name)) in commands.iter().enumerate() {
        let old = old.map_or_else(|| zero.clone(), |o| o.to_string());
        let new = new.map_or_else(|| zero.clone(), |o| o.to_string());
        let mut line = format!("{old} {new} {name}").into_bytes();
        if i == 0 {
            line.extend_from_slice(b"\0report-status side-band-64k object-format=sha1");
        }
        line.push(b'\n');
        req.extend(pkt(&line));
    }
    req.extend_from_slice(b"0000");
    if let Some(pack) = pack {
        req.extend(pack);
    }
    let body: openxet_storage::BodyStream =
        Box::pin(futures_util::stream::iter([Ok(Bytes::from(req))]));
    let mut stream = ok(engine.receive_pack(repo, 0, &principal(who), body).await);
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&ok(chunk));
    }
    let mut data = Vec::new();
    let mut messages = String::new();
    for payload in pkt_payloads(&out) {
        match payload.split_first() {
            Some((1, rest)) => data.extend_from_slice(rest),
            Some((2 | 3, rest)) => messages.push_str(&String::from_utf8_lossy(rest)),
            _ => {}
        }
    }
    let report = pkt_payloads(&data)
        .into_iter()
        .map(|l| String::from_utf8_lossy(&l).trim_end().to_owned())
        .collect();
    RawResult { report, messages }
}

/// `git pack-objects --stdout` of `objects` (no `--revs`) from `dir`.
pub fn pack_of(dir: &Path, objects: &[Oid]) -> Vec<u8> {
    let input = objects.iter().fold(String::new(), |mut s, o| {
        let _ = writeln!(s, "{o}");
        s
    });
    pack_with(dir, &["pack-objects", "-q", "--stdout"], &input)
}

/// A thin pack of `objects` (each with the path name pack-objects matches
/// bases by) that may use the tree of `edge` as delta bases, which the
/// receiver must supply: `-<edge>` is pack-objects' preferred-base line,
/// which without `--revs` (that `--thin` implies, and where only commits
/// adjacent to the sent ones are edges) yields `REF_DELTA`s against it.
pub fn pack_thin(dir: &Path, edge: Oid, objects: &[(Oid, &str)]) -> Vec<u8> {
    let mut input = format!("-{edge}\n");
    for (oid, name) in objects {
        let _ = writeln!(input, "{oid} {name}");
    }
    pack_with(dir, &["pack-objects", "-q", "--stdout"], &input)
}

/// `git pack-objects --revs --stdout` of `revs` (one per line), thin when
/// asked.
pub fn pack_revs(dir: &Path, revs: &str, thin: bool) -> Vec<u8> {
    let mut args = vec!["pack-objects", "-q", "--revs", "--stdout"];
    if thin {
        args.push("--thin");
    }
    pack_with(dir, &args, &format!("{revs}\n"))
}

fn pack_with(dir: &Path, args: &[&str], input: &str) -> Vec<u8> {
    use std::io::Write as _;
    let mut child = ok(Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn());
    if let Some(mut stdin) = child.stdin.take() {
        ok(stdin.write_all(input.as_bytes()));
    }
    let out = ok(child.wait_with_output());
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

/// The object `spec` names in `dir`.
pub fn oid(dir: &Path, spec: &str) -> Oid {
    ok(must(dir, &["rev-parse", spec]).trim().parse())
}

/// Text that deltas well: `lines` numbered lines from `seed`.
pub fn text(seed: u64, lines: usize) -> Vec<u8> {
    let mut x = seed;
    let mut out = String::new();
    for i in 0..lines {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let _ = writeln!(out, "line {i}: {x:016x} {x:020}");
    }
    out.into_bytes()
}

/// A commit in `work` whose tree is `base`'s plus `files`, with `parents`,
/// made without touching the index or HEAD.
pub fn commit_tree(work: &Path, base: &str, parents: &[Oid], files: &[(&str, Oid)]) -> Oid {
    let mut listing = must(work, &["ls-tree", base]);
    for (path, blob) in files {
        let _ = writeln!(listing, "100644 blob {blob}\t{path}");
    }
    let tree = {
        use std::io::Write as _;
        let mut child = ok(Command::new("git")
            .current_dir(work)
            .arg("mktree")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn());
        if let Some(mut stdin) = child.stdin.take() {
            ok(stdin.write_all(listing.as_bytes()));
        }
        let out = ok(child.wait_with_output());
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let mut args = vec![
        "commit-tree".to_owned(),
        tree,
        "-m".to_owned(),
        "crafted".to_owned(),
    ];
    for p in parents {
        args.push("-p".to_owned());
        args.push(p.to_string());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    ok(must(work, &args).trim().parse())
}

/// Writes `contents` as a blob in `work`.
pub fn blob(work: &Path, contents: &[u8]) -> Oid {
    use std::io::Write as _;
    let mut child = ok(Command::new("git")
        .current_dir(work)
        .args(["hash-object", "-w", "--stdin"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn());
    if let Some(mut stdin) = child.stdin.take() {
        ok(stdin.write_all(contents));
    }
    let out = ok(child.wait_with_output());
    ok(String::from_utf8_lossy(&out.stdout).trim().parse())
}
