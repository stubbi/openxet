//! What the upload, read and token tests share: a Hub with three users,
//! composed with the CAS as `openxet serve` composes them, and uploads of
//! stock `hf_xet` 1.7.0's captured xorb and shard through it.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Method, Request, StatusCode};
use openxet_auth::{Action, UrlSigner};
use openxet_cas::{AppState, BodyDeadline, CasConfig, MemoryBudget, SendDeadline};
use openxet_hub::test_support::{ALL, BEARER, Reply, TestHub, rule, user_token};
use openxet_index::{Index, Visibility};
use openxet_protocol::VerifyLimits;
use serde_json::{Value, json};
use tower::ServiceExt as _;
use url::Url;

pub const ALICE: &str = "oxt_test_alice";
pub const BOB: &str = "oxt_test_bob";
pub const CAROL: &str = "oxt_test_carol";

/// The capture's xorb (`crates/protocol/tests/hf_xet_upload.rs`).
pub const XORB: &str = "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c";

/// `a.bin` of the capture: its Xet file hash and SHA-256.
pub const A_BIN_HASH: &str = "65fef04da9e2708da8ca1acc831962d0686875eff24218d24e108362eeb420df";
pub const A_BIN_SHA256: &str = "7d4ca26bb83e603ddef686c5256eb39c7c2180008da16337a38cc21cd017127b";

/// The blob id of huggingface.co's default `.gitattributes` (S3 fixture
/// `16`).
pub const DEFAULT_GITATTRIBUTES_OID: &str = "a6344aac8c09253b3b630fb776ae94478aa0275b";

/// Alice may do everything in `alice/*`; Bob may read there; Carol has
/// no rule there.
pub async fn hub() -> TestHub {
    TestHub::start(users(), rules()).await
}

pub fn users() -> Vec<openxet_auth::StaticToken> {
    vec![
        user_token(ALICE, "alice", &[]),
        user_token(BOB, "bob", &[]),
        user_token(CAROL, "carol", &[]),
    ]
}

pub fn rules() -> Vec<openxet_auth::Rule> {
    vec![
        rule(&["alice"], &["alice/*"], ALL),
        rule(&["bob"], &["alice/*"], &[Action::Read]),
        rule(&["carol"], &["carol/*"], ALL),
    ]
}

/// The Hub and the CAS of `hub`, over its index, bucket and credentials,
/// composed into one router as `openxet serve` composes them.
pub fn with_cas(hub: &TestHub) -> Router {
    let state = &hub.state;
    let cas = AppState::new(
        Arc::clone(&state.index) as Arc<dyn Index>,
        Arc::clone(&state.bucket),
        Arc::clone(&state.auth),
        Arc::new(UrlSigner::new([7; 32], UrlSigner::DEFAULT_LIFETIME).unwrap()),
        CasConfig {
            public_url: Url::parse(openxet_hub::test_support::PUBLIC_URL).unwrap(),
            visibility: Visibility::Global,
            verify_limits: VerifyLimits::default(),
            heartbeat_interval: CasConfig::DEFAULT_HEARTBEAT_INTERVAL,
            body_deadline: BodyDeadline::DEFAULT,
            send_deadline: SendDeadline::DEFAULT,
            memory_budget: MemoryBudget::DEFAULT_BYTES,
        },
    );
    openxet_http::compose(
        vec![
            openxet_hub::mounted(state.clone()),
            openxet_cas::mounted(cas),
        ],
        BodyDeadline::DEFAULT,
    )
}

/// Sends `method path` to `router` with `token` and `body`.
pub async fn send(
    router: &Router,
    method: Method,
    path: &str,
    token: &str,
    body: Vec<u8>,
) -> Reply {
    let request = Request::builder()
        .method(method)
        .uri(path)
        .header(AUTHORIZATION, format!("{BEARER} {token}"))
        .body(Body::from(body))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Reply {
        status,
        headers,
        body,
    }
}

/// A CAS token of `kind` (`read` or `write`) for `{repo}` from the Hub,
/// as `hf_xet` fetches it.
pub async fn xet_token(hub: &TestHub, user: &str, repo: &str, kind: &str) -> String {
    let reply = hub
        .call(
            Method::GET,
            &format!("/api/models/{repo}/xet-{kind}-token/main"),
            Some(user),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    reply.json()["accessToken"].as_str().unwrap().to_owned()
}

fn capture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/hf-xet-1.7.0-upload")
        .join(name);
    std::fs::read(path).unwrap()
}

/// Uploads the capture's xorb and shard to `repo` through the CAS with a
/// write token `user` gets from the Hub, as `hf upload` does before its
/// commit; the shard registers `a.bin` and `b.bin` to the repository.
pub async fn upload_capture(hub: &TestHub, router: &Router, user: &str, repo: &str) {
    let token = xet_token(hub, user, repo, "write").await;
    let xorb = send(
        router,
        Method::POST,
        &format!("/v1/xorbs/default/{XORB}"),
        &token,
        capture(&format!("{XORB}.xorb")),
    )
    .await;
    assert_eq!(xorb.status, StatusCode::OK, "xorb: {}", xorb.text());
    let shard = send(
        router,
        Method::POST,
        "/v2/shards",
        &token,
        capture("upload.shard"),
    )
    .await;
    assert_eq!(shard.status, StatusCode::OK, "shard: {}", shard.text());
    assert!(shard.text().contains("\"result\""), "{}", shard.text());
}

/// `a.bin`'s size, as the shard registered it to `repo`.
pub async fn a_bin_size(hub: &TestHub, repo_id: &str) -> u64 {
    let sha: [u8; 32] =
        core::array::from_fn(|i| u8::from_str_radix(&A_BIN_SHA256[2 * i..2 * i + 2], 16).unwrap());
    hub.state
        .index
        .file_by_sha256(&format!("repo:{repo_id}"), &sha)
        .await
        .unwrap()
        .unwrap()
        .size
}

/// The repository id of `{namespace}/{name}`.
pub async fn repo_id(hub: &TestHub, namespace: &str, name: &str) -> String {
    hub.state
        .index
        .repo_by_name(namespace, name)
        .await
        .unwrap()
        .unwrap()
        .id
        .to_string()
}

/// A commit header line.
pub fn header(summary: &str) -> Value {
    json!({"key": "header", "value": {"summary": summary, "description": ""}})
}

/// A `file` line holding `content`.
pub fn file(path: &str, content: &[u8]) -> Value {
    use base64::Engine as _;
    let content = base64::engine::general_purpose::STANDARD.encode(content);
    json!({"key": "file", "value": {"content": content, "path": path, "encoding": "base64"}})
}

/// An `lfsFile` line.
pub fn lfs_file(path: &str, sha256: &str, size: u64) -> Value {
    json!({"key": "lfsFile", "value": {"path": path, "algo": "sha256", "oid": sha256, "size": size}})
}

/// Commits `lines` to `alice/m`'s `rev` as Alice and asserts it landed;
/// returns the commit id.
pub async fn commit_ok(hub: &TestHub, repo: &str, rev: &str, lines: &[Value]) -> String {
    let reply = hub
        .commit(ALICE, &format!("/api/models/{repo}"), rev, lines)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "commit: {}", reply.text());
    reply.json()["commitOid"].as_str().unwrap().to_owned()
}

/// A repository `alice/m` holding the S3 upload-folder commit: a README,
/// `config.json`, `sub/small.txt`, and `big.bin` as `a.bin` of the
/// capture, uploaded through Xet. Returns the composed router and the
/// commit id.
pub async fn uploaded(hub: &TestHub) -> (Router, String) {
    hub.create(ALICE, "alice", "m", None).await;
    let router = with_cas(hub);
    upload_capture(hub, &router, ALICE, "alice/m").await;
    let size = a_bin_size(hub, &repo_id(hub, "alice", "m").await).await;
    let oid = commit_ok(
        hub,
        "alice/m",
        "main",
        &[
            header("S3 upload_folder"),
            file("README.md", b"# openxet compat S3\n"),
            lfs_file("big.bin", A_BIN_SHA256, size),
            file("config.json", b"{\"hello\": \"openxet\"}\n"),
            file("sub/small.txt", b"small file\n"),
        ],
    )
    .await;
    (router, oid)
}

/// Entries of a JSON array by `path`.
pub fn by_path<'a>(entries: &'a Value, path: &str) -> &'a Value {
    entries
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["path"] == path)
        .unwrap_or_else(|| panic!("no {path} in {entries}"))
}

/// The paths of a JSON array of entries, in order.
pub fn paths(entries: &Value) -> Vec<String> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap().to_owned())
        .collect()
}
