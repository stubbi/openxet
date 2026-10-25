//! Fetch-side harness: pkt-line requests, response collection, the R22
//! scenario repository, a smart HTTP front for real `git` clients, and the
//! spawn log that tells whether git ran.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::handler::Handler as _;
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt as _;
use openxet_git::{Engine, GitError, ResponseStream, Service, protocol_version};
use openxet_index::{CommitTx, GitStore, Oid, PackState, RepoId};
use openxet_storage::BodyStream;
use tempfile::TempDir;

use super::{World, fixture, fixture_repo, git, ok, pack_objects, rev, set};

/// A pkt-line request: `"0000"` is a flush, `"0001"` a delimiter, anything
/// else a line, sent with a trailing newline.
pub fn request(lines: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for line in lines {
        match *line {
            "0000" | "0001" => out.extend_from_slice(line.as_bytes()),
            l => {
                let data = format!("{l}\n");
                out.extend_from_slice(format!("{:04x}", data.len() + 4).as_bytes());
                out.extend_from_slice(data.as_bytes());
            }
        }
    }
    out
}

/// One-chunk request body.
pub fn body(bytes: Vec<u8>) -> BodyStream {
    futures_util::stream::iter([Ok(Bytes::from(bytes))]).boxed()
}

/// Reads a response to its end, panicking on an error item.
pub async fn collect(mut stream: ResponseStream) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&ok(chunk));
    }
    out
}

/// `upload_pack` collected; panics if it returned an error.
pub async fn upload(engine: &Engine, repo: RepoId, protocol: u8, req: Vec<u8>) -> Vec<u8> {
    collect(ok(engine
        .upload_pack(repo, protocol, "tester", body(req))
        .await))
    .await
}

/// The `ERR` packet openxet sends for a want it refuses.
pub fn not_our_ref(oid: Oid) -> Vec<u8> {
    openxet_git::Refused::NotOurRef(oid).pkt().to_vec()
}

/// A v2 `fetch` request for `wants`.
pub fn v2_fetch(wants: &[Oid]) -> Vec<u8> {
    let mut lines = vec![
        "command=fetch".to_owned(),
        "object-format=sha1".to_owned(),
        "0001".to_owned(),
        "no-progress".to_owned(),
    ];
    lines.extend(wants.iter().map(|w| format!("want {w}")));
    lines.push("done".to_owned());
    lines.push("0000".to_owned());
    request(&lines.iter().map(String::as_str).collect::<Vec<_>>())
}

/// A v0/v1 request for `wants` with no haves.
pub fn v0_fetch(wants: &[Oid]) -> Vec<u8> {
    let mut lines: Vec<String> = wants.iter().map(|w| format!("want {w}")).collect();
    lines.push("0000".to_owned());
    lines.push("done".to_owned());
    request(&lines.iter().map(String::as_str).collect::<Vec<_>>())
}

/// The R22 scenario (ADR 0004 item 2), from a 30-commit `tf10k` fixture:
/// - `refs/heads/main` was pushed to the tip, then force-pushed back to
///   `main~10`, so `main~1` to `main~9` sit in a committed pack unreachable;
/// - `refs/tags/t15` is an annotated tag of `main~15`, `refs/tags/treetag`
///   one of `main~20`'s tree, `refs/tags/blob` points straight at a blob;
/// - `refs/heads/scratch` (at `main~10`) was pushed with a pack holding a
///   commit no ref reaches (`dangling`);
/// - `pending` and `lost` are commits only in a pending and a lost pack.
pub struct Scenario {
    pub fixture: TempDir,
    pub repo: RepoId,
    pub main: Oid,
    pub tip: Oid,
    pub away: Oid,
    pub dangling: Oid,
    pub pending: Oid,
    pub lost: Oid,
    pub t15: Oid,
    pub t15_target: Oid,
    pub treetag: Oid,
    pub tagged_tree: Oid,
    pub blob: Oid,
    pub other_tree: Oid,
    pub other_blob: Oid,
}

impl Scenario {
    pub fn src(&self) -> PathBuf {
        fixture_repo(&self.fixture)
    }

    pub fn at(&self, spec: &str) -> Oid {
        rev(&self.src(), spec)
    }
}

/// `git` with an identity for the objects a test writes, as no
/// configuration gives one; returns stdout.
fn git_as_tester(repo: &Path, args: &[&str]) -> String {
    let mut all = vec![
        "-c",
        "user.name=Tester",
        "-c",
        "user.email=tester@openxet.invalid",
    ];
    all.extend_from_slice(args);
    git(repo, &all)
}

pub async fn scenario(world: &World) -> Scenario {
    let fx = fixture("tf10k", 30);
    let src = fixture_repo(&fx);
    let commit_tree = |base: &str, message: &str| -> Oid {
        let tree = format!("{base}^{{tree}}");
        let args = ["commit-tree", &tree, "-p", base, "-m", message];
        ok(git_as_tester(&src, &args).trim().parse())
    };
    let dangling = commit_tree("main~5", "dangling");
    let pending = commit_tree("main~6", "pending");
    let lost = commit_tree("main~7", "lost");
    let tag = |name: &str, target: &str| {
        git_as_tester(&src, &["tag", "-a", name, target, "-m", name]);
        rev(&src, &format!("refs/tags/{name}"))
    };
    let t15 = tag("t15", "main~15");
    let treetag = tag("treetag", "main~20^{tree}");
    let blob = rev(&src, "main~25:src/dir0/file0.txt");
    let other_blob = rev(&src, "main~25:src/dir1/file1.txt");
    let main = rev(&src, "main~10");
    let tip = rev(&src, "main");

    let repo = world.repo("org", "scenario").await;
    let packs = world.dir.path().join("scenario-packs");
    let base = pack_objects(
        &src,
        "main~10\nrefs/tags/t15\nrefs/tags/treetag",
        &packs.join("base"),
    );
    world
        .commit(
            repo,
            &base,
            1_761_400_000,
            &[
                set("refs/heads/main", None, main),
                set("refs/tags/t15", None, t15),
                set("refs/tags/treetag", None, treetag),
                set("refs/tags/blob", None, blob),
            ],
        )
        .await;
    let ahead = pack_objects(&src, "main\n^main~10", &packs.join("ahead"));
    world
        .commit(
            repo,
            &ahead,
            1_761_400_001,
            &[set("refs/heads/main", Some(main), tip)],
        )
        .await;
    move_ref(world, repo, "refs/heads/main", tip, main).await;
    let side = pack_objects(&src, &format!("{dangling}\n^main"), &packs.join("side"));
    world
        .commit(
            repo,
            &side,
            1_761_400_002,
            &[set("refs/heads/scratch", None, main)],
        )
        .await;
    let p = pack_objects(&src, &format!("{pending}\n^main"), &packs.join("pending"));
    world.pending(repo, &p, 1_761_400_003).await;
    let l = pack_objects(&src, &format!("{lost}\n^main"), &packs.join("lost"));
    let row = world.pending(repo, &l, 1_761_400_004).await;
    assert!(ok(world
        .index
        .mark_pack(row.id, PackState::Pending, PackState::Lost)
        .await));

    Scenario {
        repo,
        main,
        tip,
        away: rev(&src, "main~5"),
        dangling,
        pending,
        lost,
        t15,
        t15_target: rev(&src, "main~15"),
        treetag,
        tagged_tree: rev(&src, "main~20^{tree}"),
        blob,
        other_tree: rev(&src, "main~21^{tree}"),
        other_blob,
        fixture: fx,
    }
}

/// Moves `name` from `old` to `new` with no pack, as a force push of
/// objects the repository already holds does.
pub async fn move_ref(world: &World, repo: RepoId, name: &str, old: Oid, new: Oid) {
    let generation = ok(world.index.snapshot(repo).await).full_repack_gen;
    ok(world
        .index
        .commit(CommitTx {
            repo,
            updates: &[set(name, Some(old), new)],
            pack: None,
            full_repack_gen: generation,
            actor: "tester",
            request: "test",
        })
        .await);
}

/// What the smart HTTP front saw.
#[derive(Default)]
pub struct Seen {
    /// The protocol of every `git-upload-pack` POST.
    pub protocols: Vec<u8>,
}

#[derive(Clone)]
struct Front {
    engine: Arc<Engine>,
    repo: RepoId,
    seen: Arc<Mutex<Seen>>,
}

/// Serves `repo` over a minimal smart HTTP front on 127.0.0.1 (the real
/// one is Task 15's): `info/refs` and `git-upload-pack` of `/repo.git`,
/// gzip request bodies inflated, as git sends them. Returns the clone URL.
pub async fn serve_http(engine: Arc<Engine>, repo: RepoId) -> (String, Arc<Mutex<Seen>>) {
    let seen = Arc::new(Mutex::new(Seen::default()));
    let front = Front {
        engine,
        repo,
        seen: Arc::clone(&seen),
    };
    // One handler that routes by itself: the server's route builder
    // (openxet-http) is Task 15's, and clippy keeps axum's own routing out.
    let app = route.with_state(front).into_make_service();
    let listener = ok(tokio::net::TcpListener::bind("127.0.0.1:0").await);
    let addr = ok(listener.local_addr());
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (format!("http://{addr}/repo.git"), seen)
}

fn protocol(headers: &HeaderMap) -> u8 {
    headers
        .get("git-protocol")
        .and_then(|v| protocol_version(v.as_bytes()))
        .unwrap_or(0)
}

fn failed(e: &GitError) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
}

async fn route(State(front): State<Front>, req: Request) -> Response {
    let (method, path) = (req.method().clone(), req.uri().path().to_owned());
    let query = req.uri().query().map(str::to_owned);
    let (parts, body) = req.into_parts();
    match (method, path.as_str(), query.as_deref()) {
        (Method::GET, "/repo.git/info/refs", Some("service=git-upload-pack")) => {
            info_refs(&front, &parts.headers).await
        }
        (Method::POST, "/repo.git/git-upload-pack", _) => {
            upload_pack(&front, &parts.headers, body).await
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn info_refs(front: &Front, headers: &HeaderMap) -> Response {
    let protocol = protocol(headers);
    let adv = match front
        .engine
        .advertise(front.repo, protocol, Service::UploadPack)
        .await
    {
        Ok(adv) => adv,
        Err(e) => return failed(&e),
    };
    let mut out = Vec::new();
    if protocol != 2 {
        out.extend_from_slice(&request(&["# service=git-upload-pack", "0000"]));
    }
    out.extend_from_slice(&adv);
    (
        [(
            header::CONTENT_TYPE,
            "application/x-git-upload-pack-advertisement",
        )],
        out,
    )
        .into_response()
}

async fn upload_pack(front: &Front, headers: &HeaderMap, body: Body) -> Response {
    let protocol = protocol(headers);
    if let Ok(mut seen) = front.seen.lock() {
        seen.protocols.push(protocol);
    }
    let gzip = headers
        .get(header::CONTENT_ENCODING)
        .is_some_and(|v| v.as_bytes() == b"gzip");
    let stream: BodyStream = if gzip {
        let raw = match axum::body::to_bytes(body, usize::MAX).await {
            Ok(raw) => raw,
            Err(e) => return (StatusCode::BAD_REQUEST, e.to_string()).into_response(),
        };
        let mut inflated = Vec::new();
        if let Err(e) =
            std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&raw[..]), &mut inflated)
        {
            return (StatusCode::BAD_REQUEST, e.to_string()).into_response();
        }
        futures_util::stream::iter([Ok(Bytes::from(inflated))]).boxed()
    } else {
        body.into_data_stream()
            .map(|r| r.map_err(std::io::Error::other))
            .boxed()
    };
    match front
        .engine
        .upload_pack(front.repo, protocol, "tester", stream)
        .await
    {
        Ok(response) => (
            [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
            Body::from_stream(response),
        )
            .into_response(),
        Err(GitError::Refused(r)) if r.retryable() => {
            (StatusCode::TOO_MANY_REQUESTS, r.to_string()).into_response()
        }
        Err(e) => failed(&e),
    }
}

/// A `git` client run with no user, system or proxy configuration.
pub fn client(dir: &Path, args: &[&str]) -> std::process::Output {
    ok(Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("all_proxy")
        .output())
}

/// Like [`client`], panicking with git's stderr on failure.
pub fn client_ok(dir: &Path, args: &[&str]) -> String {
    let out = client(dir, args);
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The variable naming the spawn log in a child test.
pub const SPAWN_LOG: &str = "OPENXET_TEST_SPAWN_LOG";

/// A directory holding a `git` that logs its arguments to `log` and runs
/// `real`, for a child test to put first on `PATH`.
pub fn logging_git(real: &Path) -> (TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = ok(TempDir::new());
    let log = dir.path().join("spawns.log");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
        log.display(),
        real.display()
    );
    let bin = dir.path().join("git");
    ok(std::fs::write(&bin, script));
    ok(std::fs::set_permissions(
        &bin,
        std::fs::Permissions::from_mode(0o755),
    ));
    (dir, log)
}

/// The gits a protocol test runs against: the one on `PATH`, and
/// `OPENXET_TEST_GIT_237` when set (git 2.37, which advertises
/// `object-info` unconditionally).
pub fn gits_under_test() -> Vec<PathBuf> {
    let out = ok(Command::new("sh").args(["-c", "command -v git"]).output());
    let mut gits = vec![PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    )];
    if let Some(old) = std::env::var_os("OPENXET_TEST_GIT_237") {
        gits.push(PathBuf::from(old));
    }
    gits
}

/// `PATH` with `dir` first.
pub fn path_with(dir: &Path) -> OsString {
    let mut paths = vec![dir.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    ok(std::env::join_paths(paths))
}

/// How many times the engine has started `git upload-pack` for a request
/// (not for the advertisement it captures at start-up).
pub fn upload_pack_spawns() -> usize {
    let log = std::env::var_os(SPAWN_LOG).unwrap_or_default();
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|l| *l == "upload-pack --stateless-rpc .")
        .count()
}

/// The variable marking a child test run.
const CHILD: &str = "OPENXET_GIT_TEST_CHILD";

/// Whether this is a child run of [`run_child`].
pub fn is_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

/// Runs the ignored test `name` of this binary in a child process with
/// `real` behind a logging `git` first on `PATH`, and asserts it passed.
/// The parent's environment can't be changed from a test (`set_var` is
/// `unsafe`, which the workspace forbids).
pub fn run_child_with_git(name: &str, real: &Path) {
    let (dir, log) = logging_git(real);
    let out = ok(Command::new(ok(std::env::current_exe()))
        .args([name, "--exact", "--include-ignored", "--nocapture"])
        .env(CHILD, "1")
        .env("PATH", path_with(dir.path()))
        .env(SPAWN_LOG, &log)
        .output());
    let report = format!(
        "child {name} with {} failed\nstdout:\n{}\nstderr:\n{}",
        real.display(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{report}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "{report}"
    );
}
