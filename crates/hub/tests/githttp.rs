//! Git smart HTTP (ruling M2-R11): stock `git` against the Hub's Git
//! routes on a port, the credential and repository checks of every route
//! (ruling M2-R6), the dumb protocol refused, gzip request bodies, and a
//! node with an empty cache serving what another node wrote.
//!
//! Pushes need openxet's `proc-receive` hook, so the push half runs against
//! the real `openxet serve` in `crates/openxet/tests/hub_serve.rs`; here a
//! push is only ever refused or answered by its advertisement.

#![allow(clippy::unwrap_used)]

mod support;

use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use base64::prelude::BASE64_STANDARD;
use openxet_git::{Engine, GitConfig};
use openxet_hub::test_support::{BEARER, Reply, TestHub};
use serde_json::json;
use support::{ALICE, BOB, CAROL, file, header, hub};
use tempfile::TempDir;

/// The unknown-repository answer of the Git routes, byte for byte: ruling
/// M2-R6's message and hint as `text/plain`, which git shows as `remote:`.
const UNKNOWN_BODY: &str = "Repository not found\nCheck the repository id and the token's \
     access: a repository the token may not read answers exactly like one that doesn't exist.\n";

const RECEIVE_REQUEST: (&str, &str) = ("content-type", "application/x-git-receive-pack-request");

/// The `Authorization` of HTTP Basic with `token` as the password, as git
/// sends it.
fn basic(token: &str) -> String {
    const SCHEME: &str = "Basic";
    format!(
        "{SCHEME} {}",
        BASE64_STANDARD.encode(format!("git:{token}"))
    )
}

/// `method path` with the `Authorization` value `credential`, `headers`
/// and `body`.
async fn send(
    hub: &TestHub,
    method: Method,
    path: &str,
    credential: Option<String>,
    headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> Reply {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(value) = credential {
        request = request.header(AUTHORIZATION, value);
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    hub.send(request.body(body.into()).unwrap()).await
}

async fn get(hub: &TestHub, path: &str, credential: Option<String>) -> Reply {
    send(hub, Method::GET, path, credential, &[], Body::empty()).await
}

fn assert_unknown(reply: &Reply, what: &str) {
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "{what}: {}",
        reply.text()
    );
    assert_eq!(reply.text(), UNKNOWN_BODY, "{what}");
    assert_eq!(
        reply.header("content-type"),
        Some("text/plain; charset=utf-8"),
        "{what}"
    );
    assert_eq!(reply.header("x-error-code"), Some("RepoNotFound"), "{what}");
}

/// `alice/m` with a file beside the initial commit, and the `dataset`
/// repository `alice/d`.
async fn seeded() -> TestHub {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    hub.create(ALICE, "alice", "d", Some("dataset")).await;
    for repo in ["/api/models/alice/m", "/api/datasets/alice/d"] {
        let reply = hub
            .commit(
                ALICE,
                repo,
                "main",
                &[header("add"), file("README.md", b"# hello\n")],
            )
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    }
    hub
}

/// Serves `router` on a free port of the loopback interface; the task ends
/// with the test's runtime.
async fn listen(router: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await });
    format!("http://{addr}")
}

/// `git args` in `dir` with no system or user configuration and no
/// prompt: its exit status, stdout and stderr.
async fn git(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let home = dir.join(".home");
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = tokio::process::Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            cmd.env_remove(name);
        }
    }
    let out = cmd
        .args(args)
        .current_dir(dir)
        .env("HOME", &home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(env.iter().copied())
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// `base` with `user:token@` before its host.
fn with_credential(base: &str, token: &str) -> String {
    base.replacen("http://", &format!("http://git:{token}@"), 1)
}

/// Every object and ref of the repository at `dir`, in a stable order.
async fn contents(dir: &Path) -> String {
    let (ok, objects, err) = git(
        dir,
        &["cat-file", "--batch-all-objects", "--batch-check"],
        &[],
    )
    .await;
    assert!(ok, "{err}");
    let (ok, refs, err) = git(dir, &["for-each-ref"], &[]).await;
    assert!(ok, "{err}");
    format!("{refs}\n{objects}")
}

/// `upload_pack_needs_read_and_hides_private` (design §5.9 req 2, ruling
/// M2-R6): every Git route answers a caller who may not read the
/// repository exactly as a missing one, `Bearer` or Basic alike, and a
/// request without a credential `401` with a `Basic` challenge, so git
/// asks its credential helper; a reader is served, over protocol 0 with
/// the `# service=` preamble and over protocol 2 without it, and stock
/// `git` clones with the token as the Basic password.
#[tokio::test(flavor = "multi_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "every Git route, address and credential kind, then a stock clone"
)]
async fn upload_pack_needs_read_and_hides_private() {
    let hub = seeded().await;
    let routes = [
        (Method::GET, "info/refs?service=git-upload-pack"),
        (Method::GET, "info/refs?service=git-receive-pack"),
        (Method::GET, "info/refs"),
        (Method::POST, "git-upload-pack"),
        (Method::POST, "git-receive-pack"),
    ];
    for (method, route) in &routes {
        for prefix in ["", "/datasets"] {
            for auth in [Some(format!("{BEARER} {CAROL}")), Some(basic(CAROL))] {
                let reply = |name: &'static str| {
                    let (hub, method, auth) = (&hub, method.clone(), auth.clone());
                    let path = format!("{prefix}/alice/{name}/{route}");
                    async move { send(hub, method, &path, auth, &[], Body::empty()).await }
                };
                let existing = reply(if prefix.is_empty() { "m" } else { "d" }).await;
                let what = format!("{method} {prefix}/{route}");
                assert_unknown(&existing, &what);
                for other in ["gone", "m.git", "d.git", "a..b"] {
                    let other_reply = reply(other).await;
                    assert_eq!(
                        existing.comparable(),
                        other_reply.comparable(),
                        "{what} {other}"
                    );
                }
            }
            let path = |name: &str| format!("{prefix}/alice/{name}/{route}");
            let anonymous = send(&hub, method.clone(), &path("m"), None, &[], Body::empty()).await;
            let missing = send(
                &hub,
                method.clone(),
                &path("gone"),
                None,
                &[],
                Body::empty(),
            )
            .await;
            assert_eq!(
                anonymous.status,
                StatusCode::UNAUTHORIZED,
                "{method} {route}"
            );
            assert_eq!(
                anonymous.comparable(),
                missing.comparable(),
                "{method} {route}"
            );
            let challenges: Vec<&str> = anonymous
                .headers
                .get_all("www-authenticate")
                .iter()
                .map(|v| v.to_str().unwrap())
                .collect();
            assert_eq!(
                challenges,
                [r#"Basic realm="openxet""#, r#"Bearer realm="openxet""#]
            );
            let wrong = send(
                &hub,
                method.clone(),
                &path("m"),
                Some(basic("oxt_test_wrong")),
                &[],
                Body::empty(),
            )
            .await;
            assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
            assert_eq!(
                wrong.header("x-error-message"),
                Some("Invalid credentials in Authorization header")
            );
        }
    }

    // A reader, over protocol 0 and 2, through every address of the
    // repository, with either scheme.
    for path in [
        "/alice/m",
        "/alice/m.git",
        "/datasets/alice/d",
        "/datasets/alice/d.git",
    ] {
        for auth in [format!("{BEARER} {BOB}"), basic(BOB)] {
            let v0 = get(
                &hub,
                &format!("{path}/info/refs?service=git-upload-pack"),
                Some(auth.clone()),
            )
            .await;
            assert_eq!(v0.status, StatusCode::OK, "{path}: {}", v0.text());
            assert_eq!(
                v0.header("content-type"),
                Some("application/x-git-upload-pack-advertisement")
            );
            assert_eq!(v0.header("cache-control"), Some("no-store"));
            assert!(
                v0.text().starts_with("001e# service=git-upload-pack\n0000"),
                "{}",
                v0.text()
            );
            assert!(v0.text().contains("refs/heads/main"), "{}", v0.text());
            let v2 = send(
                &hub,
                Method::GET,
                &format!("{path}/info/refs?service=git-upload-pack"),
                Some(auth),
                &[("git-protocol", "version=2")],
                Body::empty(),
            )
            .await;
            assert_eq!(v2.status, StatusCode::OK);
            assert!(v2.text().starts_with("000eversion 2\n"), "{}", v2.text());
        }
    }
    // A typed view of a repository without its label is no repository.
    let wrong_view = get(
        &hub,
        "/spaces/alice/m/info/refs?service=git-upload-pack",
        Some(basic(BOB)),
    )
    .await;
    assert_eq!(wrong_view.status, StatusCode::NOT_FOUND);
    assert_eq!(wrong_view.header("x-error-code"), Some("RepoNotFound"));

    // Stock git, the token as the Basic password, after the challenge.
    let base = listen(hub.router.clone()).await;
    let dir = TempDir::new().unwrap();
    let (ok, _, err) = git(
        dir.path(),
        &[
            "clone",
            "-q",
            &with_credential(&format!("{base}/alice/m"), BOB),
            "bob",
        ],
        &[],
    )
    .await;
    assert!(ok, "{err}");
    assert_eq!(
        std::fs::read(dir.path().join("bob/README.md")).unwrap(),
        b"# hello\n"
    );
    // A reader's push is refused, and git shows the server's reason.
    let bob = dir.path().join("bob");
    let (ok, _, err) = git(&bob, &["push", "origin", "HEAD:refs/heads/bob"], &[]).await;
    assert!(!ok);
    assert!(
        err.contains("remote: You don't have the rights to push to alice/m"),
        "{err}"
    );
    let (ok, _, err) = git(
        dir.path(),
        &[
            "clone",
            "-q",
            &with_credential(&format!("{base}/alice/m.git"), CAROL),
            "carol",
        ],
        &[],
    )
    .await;
    assert!(!ok);
    assert!(err.contains("not found"), "{err}");
    let (ok, _, err) = git(
        dir.path(),
        &["clone", "-q", &format!("{base}/alice/m"), "anonymous"],
        &[],
    )
    .await;
    assert!(!ok);
    assert!(err.contains("could not read Username"), "{err}");
}

/// `receive_pack_needs_write`: a reader may not push (`403`, before git
/// runs), a caller who may not read gets the unknown-repository answer, a
/// push over `git.max_push_bytes` is `413` before it is read, and the
/// first push to a missing repository creates it, with the view's label,
/// only for a caller who may create, read and write there: its
/// advertisement shows an empty repository and creates nothing, its `POST`
/// creates it.
#[tokio::test(flavor = "multi_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "every refusal of a push, then the creating push"
)]
async fn receive_pack_needs_write() {
    let hub = seeded().await;
    for (method, route) in [
        (Method::GET, "info/refs?service=git-receive-pack"),
        (Method::POST, "git-receive-pack"),
    ] {
        let reader = send(
            &hub,
            method.clone(),
            &format!("/alice/m.git/{route}"),
            Some(basic(BOB)),
            &[RECEIVE_REQUEST],
            Body::empty(),
        )
        .await;
        assert_eq!(reader.status, StatusCode::FORBIDDEN, "{}", reader.text());
        assert_eq!(
            reader.header("x-error-message"),
            Some("You don't have the rights to push to alice/m")
        );
        assert!(
            reader
                .text()
                .starts_with("You don't have the rights to push to alice/m\n"),
            "{}",
            reader.text()
        );
        let stranger = send(
            &hub,
            method.clone(),
            &format!("/alice/m.git/{route}"),
            Some(basic(CAROL)),
            &[RECEIVE_REQUEST],
            Body::empty(),
        )
        .await;
        assert_unknown(&stranger, route);
    }

    // A push larger than `git.max_push_bytes` is refused before it is read.
    let large = send(
        &hub,
        Method::POST,
        "/alice/m/git-receive-pack",
        Some(basic(ALICE)),
        &[RECEIVE_REQUEST, ("content-length", "20000000000")],
        Body::empty(),
    )
    .await;
    assert_eq!(
        large.status,
        StatusCode::PAYLOAD_TOO_LARGE,
        "{}",
        large.text()
    );
    assert!(large.text().contains("git lfs track"), "{}", large.text());

    // A refused first push creates nothing: too large, or an encoding git
    // never sends.
    for (header, status) in [
        (
            ("content-length", "20000000000"),
            StatusCode::PAYLOAD_TOO_LARGE,
        ),
        (("content-encoding", "br"), StatusCode::BAD_REQUEST),
    ] {
        let refused = send(
            &hub,
            Method::POST,
            "/carol/refused/git-receive-pack",
            Some(basic(CAROL)),
            &[RECEIVE_REQUEST, header],
            Body::empty(),
        )
        .await;
        assert_eq!(refused.status, status, "{}", refused.text());
        assert_eq!(
            labels_of(&hub, "refused").await.status,
            StatusCode::NOT_FOUND,
            "a refused push created the repository"
        );
    }

    // Bob may not create in `alice`: no repository appears.
    for (method, route) in [
        (Method::GET, "info/refs?service=git-receive-pack"),
        (Method::POST, "git-receive-pack"),
    ] {
        let refused = send(
            &hub,
            method,
            &format!("/alice/new/{route}"),
            Some(basic(BOB)),
            &[RECEIVE_REQUEST],
            Body::empty(),
        )
        .await;
        assert_unknown(&refused, "push to a missing repository without create");
    }
    let info = hub
        .call(Method::GET, "/api/models/alice/new", Some(ALICE), None)
        .await;
    assert_eq!(info.status, StatusCode::NOT_FOUND);
    // An upload-pack advertisement never creates one either.
    let fetch = get(
        &hub,
        "/carol/fetched/info/refs?service=git-upload-pack",
        Some(basic(CAROL)),
    )
    .await;
    assert_unknown(&fetch, "fetch of a missing repository");

    // Carol may create in `carol`: the advertisement shows an empty
    // repository and creates nothing (neither does `HEAD`); the `POST`
    // creates it with the label of the view pushed to.
    for (path, label) in [
        ("/carol/new.git", "model"),
        ("/datasets/carol/data", "dataset"),
    ] {
        let name = path.trim_end_matches(".git").rsplit('/').next().unwrap();
        let advertisement = format!("{path}/info/refs?service=git-receive-pack");
        let head = send(
            &hub,
            Method::HEAD,
            &advertisement,
            Some(basic(CAROL)),
            &[],
            Body::empty(),
        )
        .await;
        assert_eq!(head.status, StatusCode::OK);
        let ad = get(&hub, &advertisement, Some(basic(CAROL))).await;
        assert_eq!(ad.status, StatusCode::OK, "{path}: {}", ad.text());
        assert_eq!(
            ad.header("content-type"),
            Some("application/x-git-receive-pack-advertisement")
        );
        assert!(
            ad.text()
                .starts_with("001f# service=git-receive-pack\n0000"),
            "{}",
            ad.text()
        );
        assert_eq!(
            &ad.body[ad.body.len() - hub.state.engine.empty_receive_advertisement().len()..],
            hub.state.engine.empty_receive_advertisement()
        );
        assert_eq!(
            labels_of(&hub, name).await.status,
            StatusCode::NOT_FOUND,
            "the GET created {path}"
        );

        let pushed = send(
            &hub,
            Method::POST,
            &format!("{path}/git-receive-pack"),
            Some(basic(CAROL)),
            &[RECEIVE_REQUEST],
            Body::empty(),
        )
        .await;
        assert_eq!(pushed.status, StatusCode::OK, "{}", pushed.text());
        assert_eq!(
            pushed.header("content-type"),
            Some("application/x-git-receive-pack-result")
        );
        assert_eq!(pushed.header("cache-control"), Some("no-store"));
        let labels = labels_of(&hub, name).await;
        assert_eq!(labels.status, StatusCode::OK, "{}", labels.text());
        assert_eq!(labels.json()["labels"], json!([label]), "{}", labels.text());
        let again = get(&hub, &advertisement, Some(basic(CAROL))).await;
        assert_eq!(
            again.body, ad.body,
            "an empty repository advertises the same"
        );
    }
}

async fn refs_of(hub: &TestHub) -> axum::body::Bytes {
    hub.call(Method::GET, "/api/models/alice/m/refs", Some(ALICE), None)
        .await
        .body
}

async fn labels_of(hub: &TestHub, name: &str) -> Reply {
    hub.call(
        Method::GET,
        &format!("/api/openxet/v1/repos/carol/{name}/labels"),
        Some(CAROL),
        None,
    )
    .await
}

/// A repository a push created but whose push failed has no refs: the Hub
/// routes answer it as a repository without revisions, never with a `500`.
#[tokio::test(flavor = "multi_thread")]
async fn hub_routes_answer_a_repository_without_refs() {
    let hub = seeded().await;
    let created = send(
        &hub,
        Method::POST,
        "/carol/bare/git-receive-pack",
        Some(basic(CAROL)),
        &[RECEIVE_REQUEST],
        Body::empty(),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    for path in [
        "/api/models/carol/bare",
        "/api/models/carol/bare/revision/main",
        "/api/models/carol/bare/refs",
        "/api/models/carol/bare/tree/main",
        "/api/models/carol/bare/commits/main",
        "/api/models/carol/bare/xet-read-token/main",
        "/api/models/carol/bare/xet-write-token/main",
    ] {
        let reply = hub.call(Method::GET, path, Some(CAROL), None).await;
        assert!(
            !reply.status.is_server_error(),
            "{path}: {} {}",
            reply.status,
            reply.text()
        );
        if path.ends_with("tree/main") || path.ends_with("commits/main") {
            assert_eq!(
                reply.header("x-error-code"),
                Some("RevisionNotFound"),
                "{path}"
            );
        }
    }
    let paths_info = hub
        .call(
            Method::POST,
            "/api/models/carol/bare/paths-info/main",
            Some(CAROL),
            Some(&json!({"paths": ["a"]})),
        )
        .await;
    assert!(
        !paths_info.status.is_server_error(),
        "{}",
        paths_info.text()
    );
}

/// `git_posts_need_git_request_types` (design §5.9 req 2): a `POST` to a
/// Git route without git's own request `Content-Type` is `415` before its
/// body is read, as `git http-backend` answers. A browser can't send those
/// types cross-site without a CORS preflight, so a web page can't push with
/// credentials the browser cached for this origin: no ref moves and no
/// repository is created.
#[tokio::test(flavor = "multi_thread")]
async fn git_posts_need_git_request_types() {
    let hub = seeded().await;
    // Cross-site: a writer's credential on a body of another type moves
    // nothing and creates nothing.
    let before = refs_of(&hub).await;
    let zero = "0".repeat(40);
    let forged = format!(
        "{zero} {} refs/heads/forged\0report-status\n",
        "a".repeat(40)
    );
    let forged = format!("{:04x}{forged}0000", forged.len() + 4);
    for (path, content_type) in [
        ("/alice/m/git-receive-pack", "text/plain"),
        (
            "/alice/m/git-receive-pack",
            "application/x-www-form-urlencoded",
        ),
        ("/alice/m/git-upload-pack", "text/plain"),
        ("/carol/forged/git-receive-pack", "text/plain"),
        (
            "/carol/forged/git-receive-pack",
            "application/x-git-upload-pack-request",
        ),
    ] {
        let token = if path.starts_with("/carol") {
            CAROL
        } else {
            ALICE
        };
        let reply = send(
            &hub,
            Method::POST,
            path,
            Some(basic(token)),
            &[("content-type", content_type)],
            forged.clone(),
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "{path} {content_type}: {}",
            reply.text()
        );
    }
    assert_eq!(refs_of(&hub).await, before, "no ref moved");
    let forged_repo = hub
        .call(Method::GET, "/api/models/carol/forged", Some(CAROL), None)
        .await;
    assert_eq!(
        forged_repo.status,
        StatusCode::NOT_FOUND,
        "nothing was created"
    );
}

/// `dumb_protocol_not_served`: the paths a dumb client reads answer `404`,
/// and `info/refs` without a smart service names the smart protocol.
#[tokio::test(flavor = "multi_thread")]
async fn dumb_protocol_not_served() {
    let hub = seeded().await;
    for query in ["", "?service=git-upload-archive", "?service="] {
        let reply = get(
            &hub,
            &format!("/alice/m.git/info/refs{query}"),
            Some(basic(ALICE)),
        )
        .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{query}");
        assert!(
            reply.text().contains("smart HTTP only"),
            "{query}: {}",
            reply.text()
        );
    }
    for path in [
        "/alice/m.git/HEAD",
        "/alice/m.git/objects/info/packs",
        "/alice/m.git/objects/info/alternates",
        "/alice/m/objects/a6/344aac8c09253b3b630fb776ae94478aa0275b",
        "/alice/m.git/refs/heads/main",
    ] {
        let reply = get(&hub, path, Some(basic(ALICE))).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
    }
    let base = listen(hub.router.clone()).await;
    let dir = TempDir::new().unwrap();
    let (ok, _, err) = git(
        dir.path(),
        &[
            "-c",
            "protocol.version=0",
            "clone",
            "-q",
            &with_credential(&format!("{base}/alice/m.git"), ALICE),
        ],
        &[("GIT_SMART_HTTP", "0")],
    )
    .await;
    assert!(!ok, "a dumb clone must fail");
    assert!(err.contains("not found") || err.contains("404"), "{err}");
}

/// A request body sent with `Content-Encoding: gzip` is inflated, as git
/// sends a fetch request over 1 KiB (ruling M2-R11); a corrupt one fails
/// the request, and an encoding git never sends is refused.
#[tokio::test(flavor = "multi_thread")]
async fn gzip_request_bodies_are_inflated() {
    let hub = seeded().await;
    let request = b"0014command=ls-refs\n00010009peel\n0000";
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(request).unwrap();
    let gzipped = gz.finish().unwrap();
    let headers = [
        ("git-protocol", "version=2"),
        ("content-type", "application/x-git-upload-pack-request"),
    ];
    let plain = send(
        &hub,
        Method::POST,
        "/alice/m/git-upload-pack",
        Some(basic(ALICE)),
        &headers,
        request.to_vec(),
    )
    .await;
    assert_eq!(plain.status, StatusCode::OK);
    assert!(plain.text().contains("refs/heads/main"), "{}", plain.text());
    let inflated = send(
        &hub,
        Method::POST,
        "/alice/m/git-upload-pack",
        Some(basic(ALICE)),
        &[headers[0], headers[1], ("content-encoding", "gzip")],
        gzipped.clone(),
    )
    .await;
    assert_eq!(inflated.status, StatusCode::OK);
    assert_eq!(
        inflated.header("content-type"),
        Some("application/x-git-upload-pack-result")
    );
    assert_eq!(inflated.header("cache-control"), Some("no-store"));
    assert_eq!(inflated.body, plain.body);

    let corrupt = send(
        &hub,
        Method::POST,
        "/alice/m/git-upload-pack",
        Some(basic(ALICE)),
        &[headers[0], headers[1], ("content-encoding", "gzip")],
        gzipped[..gzipped.len() / 2].to_vec(),
    )
    .await;
    assert_eq!(
        corrupt.status,
        StatusCode::BAD_REQUEST,
        "{}",
        corrupt.text()
    );
    assert!(
        corrupt
            .text()
            .starts_with("the request body could not be read\n"),
        "{}",
        corrupt.text()
    );
    let brotli = send(
        &hub,
        Method::POST,
        "/alice/m/git-upload-pack",
        Some(basic(ALICE)),
        &[headers[0], headers[1], ("content-encoding", "br")],
        request.to_vec(),
    )
    .await;
    assert_eq!(brotli.status, StatusCode::BAD_REQUEST);
    assert!(
        brotli.text().contains("Content-Encoding"),
        "{}",
        brotli.text()
    );
}

/// `cold_node_serves_from_bucket`: a second node over the same index and
/// bucket, its Git cache empty, hydrates the packs another node's Hub
/// commits wrote and serves a clone identical to the first node's.
#[tokio::test(flavor = "multi_thread")]
async fn cold_node_serves_from_bucket() {
    let hub = seeded().await;
    let reply = hub
        .commit(
            ALICE,
            "/api/models/alice/m",
            "main",
            &[header("more"), file("src/lib.rs", b"fn main() {}\n")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let warm = listen(hub.router.clone()).await;

    let cache = TempDir::new().unwrap();
    let engine = Engine::start(
        Arc::clone(&hub.state.index),
        Arc::clone(&hub.state.bucket),
        GitConfig::new(cache.path().join("git-cache")),
    )
    .await
    .unwrap();
    let mut state = hub.state.clone();
    state.engine = Arc::new(engine);
    let cold = TestHub::with_state(TempDir::new().unwrap(), state);
    let cold_base = listen(cold.router.clone()).await;
    assert!(
        std::fs::read_dir(cache.path().join("git-cache/repos"))
            .unwrap()
            .next()
            .is_none(),
        "the second node starts with no pack"
    );

    let dir = TempDir::new().unwrap();
    for (base, name) in [(&warm, "warm"), (&cold_base, "cold")] {
        let (ok, _, err) = git(
            dir.path(),
            &[
                "clone",
                "-q",
                "--mirror",
                &with_credential(&format!("{base}/alice/m"), ALICE),
                name,
            ],
            &[],
        )
        .await;
        assert!(ok, "{name}: {err}");
    }
    let (warm, cold) = (
        contents(&dir.path().join("warm")).await,
        contents(&dir.path().join("cold")).await,
    );
    assert!(warm.contains("refs/heads/main"), "{warm}");
    assert_eq!(warm, cold);
}

/// The ids of `alice/m`'s commit that no ref reaches any more: committed
/// on a branch that is then deleted, so its pack stays committed.
async fn unreachable_commit(hub: &TestHub) -> String {
    let reply = hub
        .call(
            Method::POST,
            "/api/models/alice/m/branch/side",
            Some(ALICE),
            Some(&json!({})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let reply = hub
        .commit(
            ALICE,
            "/api/models/alice/m",
            "side",
            &[header("hidden"), file("hidden.txt", b"hidden\n")],
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let oid = reply.json()["commitOid"].as_str().unwrap().to_owned();
    let reply = hub
        .call(
            Method::DELETE,
            "/api/models/alice/m/branch/side",
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    oid
}

/// `pkt` as a pkt-line.
fn pkt(line: &str) -> String {
    format!("{:04x}{line}", line.len() + 4)
}

/// The HTTP route goes through R22 (ADR 0004 item 2): a fetch asking for a
/// commit no ref reaches gets git's `not our ref` error over protocol 2
/// and 0, and no pack.
#[tokio::test(flavor = "multi_thread")]
async fn upload_pack_route_checks_reachability() {
    let hub = seeded().await;
    let hidden = unreachable_commit(&hub).await;
    let v2 = format!(
        "{}0001{}{}0000",
        pkt("command=fetch\n"),
        pkt(&format!("want {hidden}\n")),
        pkt("done\n")
    );
    let v0 = format!(
        "{}0000{}",
        pkt(&format!("want {hidden} ofs-delta\n")),
        pkt("done\n")
    );
    for (protocol, body) in [("version=2", v2), ("version=0", v0)] {
        let reply = send(
            &hub,
            Method::POST,
            "/alice/m/git-upload-pack",
            Some(basic(ALICE)),
            &[
                ("git-protocol", protocol),
                ("content-type", "application/x-git-upload-pack-request"),
            ],
            body,
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{protocol}");
        let text = reply.text();
        assert!(
            text.contains("ERR") && text.contains("not our ref") && text.contains(&hidden),
            "{protocol}: {text}"
        );
        assert!(!text.contains("PACK"), "{protocol}: no pack is sent");
    }
}
