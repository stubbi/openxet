//! The Git LFS endpoint under `{repo}.git/info/lfs` for `git-lfs`,
//! `git-xet` and Xet-native clients (ADRs 0003 and 0005, ruling M2-R10),
//! and the capability route its upload `href`s name.

#![allow(clippy::unwrap_used)]

mod support;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use axum::body::Body;
use axum::http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use openxet_http::Clock;
use openxet_hub::test_support::{BEARER, PUBLIC_URL, Reply, TestHub};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{
    A_BIN_HASH, A_BIN_SHA256, ALICE, BOB, CAROL, a_bin_size, commit_ok, file, header, hub, repo_id,
    upload_capture, with_cas,
};

/// The scheme of HTTP Basic credentials, which git and its credential
/// helpers send with the token as the password (ADR 0005 item 2).
const BASIC: &str = "Basic";

/// The LFS media type of every batch request and answer.
const LFS_JSON: &str = "application/vnd.git-lfs+json";

/// The object of S7's `git-xet` push to huggingface.co (fixture
/// `hf/02-push-xet-lfs371--03`).
const HF_OID: &str = "474f85995709fe1d8895e50599a7252daaa0cbb2dfc4078d19416656245015d3";

// Credential header names, as constants so the fixture redaction guard
// doesn't read a list of them as a header and its value.
const AUTHORIZATION_HEADER: &str = "Authorization";
const ACCESS_TOKEN_HEADER: &str = "X-Xet-Access-Token";

/// A SHA-256 no test registers.
const UNKNOWN_OID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// The time a [`ManualClock`] starts at: 2026-10-23, as S7's captures.
const T0: u64 = 1_792_736_000;

/// A clock tests move by hand.
struct ManualClock(AtomicU64);

impl ManualClock {
    fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// [`hub`] on a [`ManualClock`] at [`T0`].
async fn hub_at_t0() -> (TestHub, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock(AtomicU64::new(T0)));
    let TestHub { state, dir, .. } = hub().await;
    let hub = TestHub::with_state(dir, state.with_clock(Arc::clone(&clock) as Arc<dyn Clock>));
    (hub, clock)
}

/// How a request carries its credential.
#[derive(Clone, Copy, Debug)]
enum Auth<'a> {
    None,
    /// HTTP Basic, the token as the password and any user name.
    Basic(&'a str),
    Bearer(&'a str),
    /// The header exactly as given.
    Raw(&'a str),
}

fn basic_value(token: &str) -> String {
    format!("{BASIC} {}", STANDARD.encode(format!("git:{token}")))
}

async fn lfs(
    hub: &TestHub,
    method: Method,
    path: &str,
    auth: Auth<'_>,
    body: Option<&Value>,
) -> Reply {
    let mut request = Request::builder().method(method).uri(path);
    request = match auth {
        Auth::None => request,
        Auth::Basic(token) => request.header(AUTHORIZATION, basic_value(token)),
        Auth::Bearer(token) => request.header(AUTHORIZATION, format!("{BEARER} {token}")),
        Auth::Raw(value) => request.header(AUTHORIZATION, value),
    };
    let body = match body {
        Some(json) => {
            request = request
                .header(ACCEPT, LFS_JSON)
                .header(CONTENT_TYPE, format!("{LFS_JSON}; charset=utf-8"));
            Body::from(serde_json::to_vec(json).unwrap())
        }
        None => Body::empty(),
    };
    hub.send(request.body(body).unwrap()).await
}

/// A batch request as `git-lfs` 3.7.1 sends it (S7), offering `transfers`.
fn batch_body(operation: &str, transfers: &[&str], objects: &[(&str, u64)]) -> Value {
    json!({
        "operation": operation,
        "objects": objects
            .iter()
            .map(|(oid, size)| json!({"oid": oid, "size": size}))
            .collect::<Vec<_>>(),
        "transfers": transfers,
        "ref": {"name": "refs/heads/main"},
        "hash_algo": "sha256",
    })
}

async fn batch(
    hub: &TestHub,
    repo: &str,
    auth: Auth<'_>,
    operation: &str,
    transfers: &[&str],
    objects: &[(&str, u64)],
) -> Reply {
    lfs(
        hub,
        Method::POST,
        &format!("/{repo}.git/info/lfs/objects/batch"),
        auth,
        Some(&batch_body(operation, transfers, objects)),
    )
    .await
}

/// `git-lfs` 3.7.1 with `git-xet` registered offers these (S7).
const WITH_XET: &[&str] = &["lfs-standalone-file", "xet", "basic", "ssh"];
/// Plain `git-lfs` 3.7.1 offers these (S7).
const WITHOUT_XET: &[&str] = &["lfs-standalone-file", "basic", "ssh"];

/// The claims of a CAS token.
fn claims(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

/// The path and query of an absolute `href` this server issued.
fn local(href: &str) -> &str {
    href.strip_prefix(PUBLIC_URL)
        .unwrap_or_else(|| panic!("{href} is not under {PUBLIC_URL}"))
}

fn header_keys(object: &Value) -> BTreeSet<String> {
    object.as_object().unwrap().keys().cloned().collect()
}

/// An upload batch to `repo` of S7's object, offering `xet`, as
/// `git-xet` 0.2.1 with `git-lfs` 3.7.1 sends it.
async fn upload_hf_object(hub: &TestHub, repo: &str, auth: Auth<'_>) -> Reply {
    batch(hub, repo, auth, "upload", WITH_XET, &[(HF_OID, 300_000)]).await
}

/// The upload action of the first object of a batch answer.
fn upload_action(reply: &Reply) -> Value {
    reply.json()["objects"][0]["actions"]["upload"].clone()
}

/// Asserts that `object` is S7's object with the `xet` upload action of
/// repository `id`: a capability `href`, the batch's CAS token in ADR 0005
/// item 10's casing, and a `verify` action without a credential.
fn assert_xet_upload(object: &Value, id: &str) {
    assert_eq!(object["oid"], HF_OID);
    assert_eq!(object["size"], 300_000);
    assert_eq!(object["authenticated"], true);
    let upload = &object["actions"]["upload"];
    let href = upload["href"].as_str().unwrap();
    let capability = format!("{PUBLIC_URL}/v1/capabilities/{id}/write?exp=");
    assert!(href.starts_with(&capability), "{href}");
    assert!(href.contains("&sig="), "{href}");
    // The capability lives 60 minutes, `hub.capability_ttl`.
    assert_eq!(upload["expires_in"], 3600);
    assert!(href.contains(&format!("exp={}&", T0 + 3600)), "{href}");
    // Exactly the casing of ADR 0005 item 10, as `git-xet` reads it.
    let headers = &upload["header"];
    assert_eq!(
        header_keys(headers),
        BTreeSet::from(
            [
                "X-Xet-Cas-Url",
                ACCESS_TOKEN_HEADER,
                "X-Xet-Token-Expiration",
                "X-Xet-Session-Id",
            ]
            .map(str::to_owned)
        )
    );
    assert_eq!(headers["X-Xet-Cas-Url"], PUBLIC_URL);
    // An epoch `hub.token_ttl` (15 minutes) ahead, as a string.
    assert_eq!(headers["X-Xet-Token-Expiration"], (T0 + 900).to_string());
    let token = headers["X-Xet-Access-Token"].as_str().unwrap();
    let claims = claims(token);
    assert_eq!(claims["scope"], format!("repo:{id}"));
    assert_eq!(claims["mode"], "write");
    assert_eq!(claims["tenant"], "alice");
    assert_eq!(claims["sub"], "alice");
    assert!(!headers["X-Xet-Session-Id"].as_str().unwrap().is_empty());
    // The verify action names the endpoint and carries no credential:
    // `git-lfs` attaches the one it holds for the host (§5.9 req 6).
    let verify = &object["actions"]["verify"];
    assert_eq!(
        verify["href"],
        format!("{PUBLIC_URL}/alice/m.git/info/lfs/objects/verify")
    );
    assert!(verify.get("header").is_none(), "{verify}");
}

#[tokio::test]
async fn upload_batch_answers_xet_with_capability_href() {
    let (hub, _clock) = hub_at_t0().await;
    hub.create(ALICE, "alice", "m", None).await;
    let id = repo_id(&hub, "alice", "m").await;
    // The request of S7's `git-xet` push to huggingface.co, as sent.
    let fixture: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../compat/fixtures/lfs/hf/02-push-xet-lfs371--03-POST-lfs-batch.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let request = &fixture["request"]["body"]["json"];
    let expected = &fixture["response"]["body"]["json"];
    for repo in ["alice/m.git", "alice/m"] {
        let reply = lfs(
            &hub,
            Method::POST,
            &format!("/{repo}/info/lfs/objects/batch"),
            Auth::Basic(ALICE),
            Some(request),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{repo}: {}", reply.text());
        assert_eq!(reply.header("content-type"), Some(LFS_JSON));
        assert_eq!(reply.header("cache-control"), Some("no-store"));
        let body = reply.json();
        openxet_hub::test_support::assert_read_keys(
            expected,
            &body,
            &[
                "transfer",
                "objects[].oid",
                "objects[].size",
                "objects[].authenticated",
                "objects[].actions.upload.href",
                "objects[].actions.upload.header",
                "objects[].actions.verify.href",
            ],
        );
        assert_eq!(body["transfer"], "xet");
        assert_xet_upload(&body["objects"][0], &id);
    }
    // Each batch is its own transfer session.
    let first = upload_action(&upload_hf_object(&hub, "alice/m", Auth::Bearer(ALICE)).await);
    let second = upload_action(&upload_hf_object(&hub, "alice/m", Auth::Bearer(ALICE)).await);
    assert_ne!(
        first["header"]["X-Xet-Session-Id"],
        second["header"]["X-Xet-Session-Id"]
    );
}

#[tokio::test]
async fn upload_batch_skips_objects_already_uploaded() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/m").await;
    let size = a_bin_size(&hub, &repo_id(&hub, "alice", "m").await).await;
    for transfers in [WITH_XET, WITHOUT_XET] {
        let reply = batch(
            &hub,
            "alice/m",
            Auth::Basic(ALICE),
            "upload",
            transfers,
            &[(A_BIN_SHA256, size), (HF_OID, 300_000)],
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        let objects = reply.json()["objects"].clone();
        // The LFS batch API: an object the server has carries no actions.
        assert_eq!(objects[0]["oid"], A_BIN_SHA256);
        assert!(objects[0].get("actions").is_none(), "{objects}");
        assert!(objects[0].get("error").is_none(), "{objects}");
        assert!(
            objects[1].get("actions").is_some() || objects[1].get("error").is_some(),
            "{objects}"
        );
    }
}

#[tokio::test]
async fn download_batch_always_basic() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/m").await;
    let id = repo_id(&hub, "alice", "m").await;
    let size = a_bin_size(&hub, &id).await;
    // `git xet install` registers `git-xet` for downloads too, which it
    // can't do (ADR 0003 decision 3): `xet` offered or not, `basic`.
    for transfers in [WITH_XET, WITHOUT_XET, &["xet"][..], &[][..]] {
        let reply = batch(
            &hub,
            "alice/m",
            Auth::Basic(BOB),
            "download",
            transfers,
            &[(A_BIN_SHA256, size)],
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{transfers:?}: {}",
            reply.text()
        );
        assert_eq!(reply.header("content-type"), Some(LFS_JSON));
        let body = reply.json();
        assert_eq!(body["transfer"], "basic", "{transfers:?}");
        let object = &body["objects"][0];
        assert_eq!(object["oid"], A_BIN_SHA256);
        assert_eq!(object["size"], size);
        // No `authenticated`: `git-lfs` sends the credential it holds for
        // the host to the gateway (ruling M2-R9).
        assert!(object.get("authenticated").is_none(), "{object}");
        let download = &object["actions"]["download"];
        assert_eq!(
            download["href"],
            format!("{PUBLIC_URL}/v1/transfer/files/{id}/{A_BIN_HASH}")
        );
        assert_eq!(download["header"], json!({ "X-Xet-Hash": A_BIN_HASH }));
    }
    // The `href` serves the file to the credential `git-lfs` holds for the
    // host, HTTP Basic, and the header it forwards is ignored.
    let href = format!("/v1/transfer/files/{id}/{A_BIN_HASH}");
    let request = Request::builder()
        .uri(href)
        .header(AUTHORIZATION, basic_value(BOB))
        .header("X-Xet-Hash", A_BIN_HASH)
        .body(Body::empty())
        .unwrap();
    let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let content = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(content.len() as u64, size);
    assert_eq!(support::files::hex(&Sha256::digest(&content)), A_BIN_SHA256);
}

#[tokio::test]
async fn lfs_download_action_has_no_token() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/m").await;
    let size = a_bin_size(&hub, &repo_id(&hub, "alice", "m").await).await;
    for auth in [Auth::Basic(ALICE), Auth::Bearer(ALICE)] {
        let reply = batch(
            &hub,
            "alice/m",
            auth,
            "download",
            WITH_XET,
            &[(A_BIN_SHA256, size), (UNKNOWN_OID, 1)],
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        // Neither the caller's credential nor any CAS token: the download
        // action's header is `X-Xet-Hash` only (ADR 0003 decision 3).
        let text = reply.text();
        assert!(!text.contains(ALICE), "{text}");
        assert!(
            !text.contains(&STANDARD.encode(format!("git:{ALICE}"))),
            "{text}"
        );
        for absent in [
            AUTHORIZATION_HEADER,
            ACCESS_TOKEN_HEADER,
            "accessToken",
            "eyJ",
        ] {
            assert!(!text.contains(absent), "{absent} in {text}");
        }
        for (name, value) in &reply.headers {
            let value = String::from_utf8_lossy(value.as_bytes());
            assert!(!value.contains(ALICE), "{name}");
            assert!(!value.contains("eyJ"), "{name}");
        }
    }
}

#[tokio::test]
async fn capability_href_refreshes_without_authorization() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let id = repo_id(&hub, "alice", "m").await;
    let reply = upload_hf_object(&hub, "alice/m", Auth::Basic(ALICE)).await;
    let href = upload_action(&reply)["href"].as_str().unwrap().to_owned();
    // `git-xet` refreshes by a GET on the `href`, with no credential when
    // `openxet setup git` set `access=none` (ADR 0003 decision 1), twice
    // in a long upload.
    for _ in 0..2 {
        let refreshed = lfs(&hub, Method::GET, local(&href), Auth::None, None).await;
        assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.text());
        assert_eq!(refreshed.header("cache-control"), Some("no-store"));
        let body = refreshed.json();
        let token = body["accessToken"].as_str().unwrap();
        assert_eq!(body["casUrl"], PUBLIC_URL);
        assert_eq!(refreshed.header("x-xet-access-token"), Some(token));
        assert_eq!(refreshed.header("x-xet-cas-url"), Some(PUBLIC_URL));
        assert_eq!(
            refreshed.header("x-xet-token-expiration").unwrap(),
            body["exp"].to_string()
        );
        let claims = claims(token);
        assert_eq!(claims["scope"], format!("repo:{id}"));
        assert_eq!(claims["mode"], "write");
        assert_eq!(claims["tenant"], "alice");
        assert_eq!(claims["exp"], body["exp"]);
        // The pusher's subject, signed into the capability, so the audit
        // log attributes the refresh to them.
        assert_eq!(claims["sub"], "alice");
    }
}

/// Every line a subscriber writes, for tests that check what the server
/// logs.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

#[tokio::test]
async fn capability_ignores_and_never_logs_authorization() {
    let logs = Logs::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // A `tokio::test` runs on this thread, so the default covers the server.
    let _guard = tracing::subscriber::set_default(subscriber);

    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let reply = upload_hf_object(&hub, "alice/m", Auth::Basic(ALICE)).await;
    let href = upload_action(&reply)["href"].as_str().unwrap().to_owned();
    let path = local(&href);
    let sig = href.split("sig=").nth(1).unwrap().to_owned();
    let garbage = "oxt_test_garbage_never_configured";

    // A valid signature with any Authorization header, valid or not:
    // the header is ignored (ADR 0003 decision 2).
    let garbage_bearer = format!("{BEARER} {garbage}");
    let garbage_basic = basic_value(garbage);
    for auth in [
        Auth::Raw(&garbage_bearer),
        Auth::Raw(&garbage_basic),
        Auth::Raw("nonsense"),
        Auth::Bearer(BOB),
    ] {
        let reply = lfs(&hub, Method::GET, path, auth, None).await;
        assert_eq!(reply.status, StatusCode::OK, "{auth:?}: {}", reply.text());
    }
    // A bad signature fails, even with a credential that may write.
    let mut forged = sig.clone();
    let last = if forged.ends_with('A') { "B" } else { "A" };
    forged.replace_range(forged.len() - 1.., last);
    let tampered = path.replace(&sig, &forged);
    for auth in [Auth::Basic(ALICE), Auth::Bearer(ALICE), Auth::None] {
        let reply = lfs(&hub, Method::GET, &tampered, auth, None).await;
        assert_eq!(
            reply.status,
            StatusCode::FORBIDDEN,
            "{auth:?}: {}",
            reply.text()
        );
        assert_eq!(reply.json()["code"], "capability_refused");
        assert!(!reply.text().contains(&forged));
        assert!(!reply.text().contains(&sig));
    }
    // The capability for another operation or repository is refused alike.
    let read = path.replace("/write?", "/read?");
    let reply = lfs(&hub, Method::GET, &read, Auth::None, None).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text());
    // So is one presented for another subject.
    assert!(path.contains("sub=alice&"), "{path}");
    let other = path.replace("sub=alice&", "sub=bob&");
    let reply = lfs(&hub, Method::GET, &other, Auth::None, None).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text());

    let logged = logs.text();
    // Each request records only whether a header came (ADR 0003 decision 2).
    assert_eq!(
        logged.matches("authorization_present=true").count(),
        6,
        "{logged}"
    );
    assert_eq!(
        logged.matches("authorization_present=false").count(),
        3,
        "{logged}"
    );
    assert!(
        !logged.is_empty(),
        "nothing was logged; the capture is broken"
    );
    for secret in [garbage, ALICE, BOB, sig.as_str(), forged.as_str()] {
        assert!(!logged.contains(secret), "{secret} logged:\n{logged}");
    }
    for encoded in [
        STANDARD.encode(format!("git:{garbage}")),
        STANDARD.encode(format!("git:{ALICE}")),
    ] {
        assert!(
            !logged.contains(&encoded),
            "Basic credential logged:\n{logged}"
        );
    }
    assert!(!logged.contains("eyJ"), "a CAS token logged:\n{logged}");
}

#[tokio::test]
async fn capability_href_outlives_first_token() {
    let (hub, clock) = hub_at_t0().await;
    hub.create(ALICE, "alice", "m", None).await;
    let reply = upload_hf_object(&hub, "alice/m", Auth::Basic(ALICE)).await;
    let upload = upload_action(&reply);
    let first_exp: u64 = upload["header"]["X-Xet-Token-Expiration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(first_exp, T0 + 900);
    let href = upload["href"].as_str().unwrap().to_owned();

    // The batch's CAS token has expired; the `href` still answers, with a
    // token that lives `hub.token_ttl` from now (Review Focus 5).
    clock.set(first_exp + 100);
    let refreshed = lfs(&hub, Method::GET, local(&href), Auth::None, None).await;
    assert_eq!(refreshed.status, StatusCode::OK, "{}", refreshed.text());
    assert_eq!(refreshed.json()["exp"], first_exp + 100 + 900);

    // A token never outlives the capability that issued it (ADR 0005 item
    // 10).
    clock.set(T0 + 3000);
    let late = lfs(&hub, Method::GET, local(&href), Auth::None, None).await;
    assert_eq!(late.status, StatusCode::OK, "{}", late.text());
    assert_eq!(late.json()["exp"], T0 + 3600);

    // At its own expiry the capability is refused; a new batch gives a
    // new one.
    clock.set(T0 + 3600);
    let expired = lfs(&hub, Method::GET, local(&href), Auth::Basic(ALICE), None).await;
    assert_eq!(expired.status, StatusCode::FORBIDDEN, "{}", expired.text());
    assert_eq!(expired.json()["code"], "capability_refused");
    assert!(
        expired.json()["message"]
            .as_str()
            .unwrap()
            .contains("expired")
    );
}

#[tokio::test]
async fn batch_without_xet_answers_object_error_501() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    for transfers in [WITHOUT_XET, &["basic"][..]] {
        let reply = batch(
            &hub,
            "alice/m",
            Auth::Basic(ALICE),
            "upload",
            transfers,
            &[(HF_OID, 300_000), (UNKNOWN_OID, 7)],
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        let body = reply.json();
        assert_eq!(body["transfer"], "basic");
        for object in body["objects"].as_array().unwrap() {
            // Never a `basic` href that fails later (ruling M2-R10).
            assert!(object.get("actions").is_none(), "{object}");
            assert_eq!(object["error"]["code"], 501, "{object}");
            let message = object["error"]["message"].as_str().unwrap();
            assert!(message.contains("git-xet"), "{message}");
            assert!(message.contains("git xet install"), "{message}");
        }
    }
}

#[tokio::test]
async fn batch_unknown_oid_is_object_404() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    hub.create(ALICE, "alice", "other", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/other").await;
    let size = a_bin_size(&hub, &repo_id(&hub, "alice", "other").await).await;
    let reply = batch(
        &hub,
        "alice/m",
        Auth::Basic(ALICE),
        "download",
        WITH_XET,
        &[
            (UNKNOWN_OID, 1),
            // Registered to another repository only: unknown here.
            (A_BIN_SHA256, size),
            ("not-a-sha256", 1),
        ],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let objects = reply.json()["objects"].clone();
    for (i, code) in [404, 404, 422].into_iter().enumerate() {
        let object = &objects[i];
        assert!(object.get("actions").is_none(), "{object}");
        assert_eq!(object["error"]["code"], code, "{object}");
        assert!(object["error"]["message"].is_string(), "{object}");
    }
    // The size is part of the object: the right SHA-256 with another size
    // is unknown too.
    upload_capture(&hub, &router, ALICE, "alice/m").await;
    let reply = batch(
        &hub,
        "alice/m",
        Auth::Basic(ALICE),
        "download",
        WITH_XET,
        &[(A_BIN_SHA256, size + 1)],
    )
    .await;
    assert_eq!(reply.json()["objects"][0]["error"]["code"], 404);
    // A hash algorithm other than SHA-256: `409`, as the batch API says.
    let mut body = batch_body("download", WITH_XET, &[(A_BIN_SHA256, size)]);
    body["hash_algo"] = "sha512".into();
    let reply = lfs(
        &hub,
        Method::POST,
        "/alice/m.git/info/lfs/objects/batch",
        Auth::Basic(ALICE),
        Some(&body),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.text());
}

/// The repository `{namespace}/{name}`, if it exists.
async fn stored(hub: &TestHub, namespace: &str, name: &str) -> Option<openxet_index::Repo> {
    hub.state.index.repo_by_name(namespace, name).await.unwrap()
}

#[tokio::test]
async fn first_push_creates_repo_when_create_allowed() {
    let hub = hub().await;
    // `git push` runs the LFS pre-push upload before Git's own push, so
    // the batch creates the repository (ruling M2-R11)…
    let reply = upload_hf_object(&hub, "alice/new", Auth::Basic(ALICE)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.json()["transfer"], "xet");
    assert_eq!(
        stored(&hub, "alice", "new").await.unwrap().labels,
        ["model"]
    );
    // …without an initial commit, which would make the push of `main` a
    // conflict.
    let refs = hub
        .call(Method::GET, "/api/models/alice/new/refs", Some(ALICE), None)
        .await;
    assert_eq!(refs.status, StatusCode::OK, "{}", refs.text());
    assert_eq!(refs.json()["branches"], json!([]));
    // Through a typed Git URL, with that type's label.
    let reply = upload_hf_object(&hub, "datasets/alice/data", Auth::Basic(ALICE)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(
        stored(&hub, "alice", "data").await.unwrap().labels,
        ["dataset"]
    );
    // A download creates nothing.
    let reply = batch(
        &hub,
        "alice/absent",
        Auth::Basic(ALICE),
        "download",
        WITH_XET,
        &[(HF_OID, 1)],
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text());
    assert!(stored(&hub, "alice", "absent").await.is_none());
    // Nor does an upload batch the server refuses.
    let mut body = batch_body("upload", WITH_XET, &[(HF_OID, 1)]);
    body["hash_algo"] = "sha512".into();
    let reply = lfs(
        &hub,
        Method::POST,
        "/alice/refused.git/info/lfs/objects/batch",
        Auth::Basic(ALICE),
        Some(&body),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.text());
    assert!(stored(&hub, "alice", "refused").await.is_none());
}

#[tokio::test]
async fn first_push_without_create_creates_nothing() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    // Bob may read `alice/*` but not create there, Carol neither: the
    // unknown-repository answer, and nothing created.
    for user in [BOB, CAROL] {
        let reply = upload_hf_object(&hub, "alice/other", Auth::Basic(user)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text());
        assert_eq!(reply.json()["code"], "repo_not_found");
    }
    assert!(stored(&hub, "alice", "other").await.is_none());
    // Bob may read `alice/m` but not write to it.
    let reply = upload_hf_object(&hub, "alice/m", Auth::Basic(BOB)).await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text());
    // Without a credential: `401` asking for Basic credentials, which git
    // gets from its credential helpers.
    let reply = upload_hf_object(&hub, "alice/new", Auth::None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text());
    assert_eq!(
        reply.header("lfs-authenticate"),
        Some(r#"Basic realm="openxet""#)
    );
    assert!(stored(&hub, "alice", "new").await.is_none());
}

#[tokio::test]
async fn verify_answers_only_for_registered_objects() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/m").await;
    let size = a_bin_size(&hub, &repo_id(&hub, "alice", "m").await).await;
    let verify = |auth, oid: &'static str, size: u64| {
        let hub = &hub;
        async move {
            lfs(
                hub,
                Method::POST,
                "/alice/m.git/info/lfs/objects/verify",
                auth,
                Some(&json!({"oid": oid, "size": size})),
            )
            .await
        }
    };
    let ok = verify(Auth::Basic(ALICE), A_BIN_SHA256, size).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    assert_eq!(ok.json(), json!({}));
    // An upload that never completed is no silent success.
    let missing = verify(Auth::Basic(ALICE), HF_OID, 300_000).await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.text());
    assert!(missing.json()["hint"].as_str().unwrap().contains("push"));
    // Verifying is part of uploading: it needs `write`.
    let bob = verify(Auth::Basic(BOB), A_BIN_SHA256, size).await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN, "{}", bob.text());
}

/// `GET /{repo}.git/info/lfs/xet-{kind}-token` with `auth`.
async fn lfs_token(hub: &TestHub, repo: &str, kind: &str, auth: Auth<'_>) -> Reply {
    let path = format!("/{repo}.git/info/lfs/xet-{kind}-token");
    lfs(hub, Method::GET, &path, auth, None).await
}

/// Asserts `reply` is `status` with ADR 0005 item 5's error body.
fn assert_error(reply: &Reply, status: StatusCode, what: &str) {
    assert_eq!(reply.status, status, "{what}: {}", reply.text());
    let body = reply.json();
    assert_eq!(body["schema"], "openxet.error.v1", "{what}");
    for key in ["code", "message", "hint"] {
        assert!(body[key].is_string(), "{what}: {key} in {body}");
    }
}

#[tokio::test]
async fn lfs_token_routes_match_adr_0005() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let id = repo_id(&hub, "alice", "m").await;
    for auth in [Auth::Basic(ALICE), Auth::Bearer(ALICE)] {
        for kind in ["read", "write"] {
            let reply = lfs_token(&hub, "alice/m", kind, auth).await;
            assert_eq!(
                reply.status,
                StatusCode::OK,
                "{auth:?} {kind}: {}",
                reply.text()
            );
            assert_eq!(reply.header("cache-control"), Some("no-store"));
            let body = reply.json();
            let token = body["accessToken"].as_str().unwrap();
            assert!(body["exp"].is_u64(), "{body}");
            assert_eq!(body["casUrl"], PUBLIC_URL);
            assert_eq!(reply.header("x-xet-access-token"), Some(token));
            assert_eq!(reply.header("x-xet-cas-url"), Some(PUBLIC_URL));
            assert_eq!(
                reply.header("x-xet-token-expiration").unwrap(),
                body["exp"].to_string()
            );
            let claims = claims(token);
            assert_eq!(claims["scope"], format!("repo:{id}"));
            assert_eq!(claims["mode"], kind);
        }
    }
    // Item 5's errors, each with the `openxet.error.v1` body.
    let missing = lfs_token(&hub, "alice/m", "read", Auth::None).await;
    assert_error(&missing, StatusCode::UNAUTHORIZED, "no credential");
    for challenge in ["lfs-authenticate", "www-authenticate"] {
        assert_eq!(missing.header(challenge), Some(r#"Basic realm="openxet""#));
    }
    let wrong = lfs_token(&hub, "alice/m", "read", Auth::Basic("oxt_test_wrong")).await;
    assert_error(&wrong, StatusCode::UNAUTHORIZED, "wrong credential");
    assert!(!wrong.text().contains("oxt_test_wrong"));
    let malformed_value = format!("{BASIC} %%%");
    let malformed = lfs_token(&hub, "alice/m", "read", Auth::Raw(&malformed_value)).await;
    assert_error(&malformed, StatusCode::UNAUTHORIZED, "malformed Basic");
    for auth in [Auth::Basic(BOB), Auth::Bearer(BOB)] {
        let read = lfs_token(&hub, "alice/m", "read", auth).await;
        assert_eq!(read.status, StatusCode::OK, "{}", read.text());
        let write = lfs_token(&hub, "alice/m", "write", auth).await;
        assert_error(&write, StatusCode::FORBIDDEN, "read-only write token");
    }
    // A repository Carol may not read answers as one that doesn't exist.
    let hidden = lfs_token(&hub, "alice/m", "read", Auth::Basic(CAROL)).await;
    let absent = lfs_token(&hub, "alice/none", "read", Auth::Basic(CAROL)).await;
    assert_error(&hidden, StatusCode::NOT_FOUND, "unreadable");
    assert_eq!(hidden.comparable(), absent.comparable());
    // A token route never creates a repository.
    let absent = lfs_token(&hub, "alice/none", "write", Auth::Basic(ALICE)).await;
    assert_error(&absent, StatusCode::NOT_FOUND, "missing");
    assert!(stored(&hub, "alice", "none").await.is_none());
}

/// `value` with the expectations' placeholders replaced from `values`
/// (the fixture's `source` lists them).
fn substitute(value: &Value, values: &[(&str, Value)]) -> Value {
    match value {
        Value::String(text) => values
            .iter()
            .find(|(key, _)| key == text)
            .map_or_else(|| value.clone(), |(_, v)| v.clone()),
        Value::Array(items) => items.iter().map(|v| substitute(v, values)).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(k, v)| (k.clone(), substitute(v, values)))
            .collect(),
        other => other.clone(),
    }
}

/// The value at `dotted` in `body`; numeric parts index lists.
fn at<'a>(body: &'a Value, dotted: &str) -> &'a Value {
    dotted.split('.').fold(body, |value, part| match value {
        Value::Array(items) => &items[part.parse::<usize>().unwrap()],
        other => &other[part],
    })
}

/// Whether `got` is `expected`, or of the kind an `<…>` pattern names.
fn matches(got: &Value, expected: &Value) -> bool {
    match expected.as_str() {
        Some("<string>") => got.as_str().is_some_and(|s| !s.is_empty()),
        Some("<int>") => got.is_i64() || got.is_u64(),
        Some("<future-epoch>") => {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            got.as_u64().is_some_and(|exp| exp > now)
        }
        Some("<http-url>") => got
            .as_str()
            .is_some_and(|s| s.starts_with("http://") || s.starts_with("https://")),
        _ => got == expected,
    }
}

/// The credential cases name `wrong-basic` send.
const JJ_WRONG: &str = "oxt_test_jj_wrong";

/// `case`'s request against the LFS URL `lfs_path`, the placeholders
/// filled from `values`.
fn jj_request(case: &Value, lfs_path: &str, values: &[(&str, Value)]) -> Request<Body> {
    let spec = &case["request"];
    let mut request = Request::builder()
        .method(spec["method"].as_str().unwrap())
        .uri(format!("{lfs_path}/{}", spec["path"].as_str().unwrap()))
        .header(ACCEPT, LFS_JSON);
    let password = match spec["auth"].as_str().unwrap() {
        "basic" => Some(ALICE),
        "wrong-basic" => Some(JJ_WRONG),
        "none" => None,
        other => panic!("{}: unknown auth {other}", case["name"]),
    };
    if let Some(password) = password {
        // jj-xet's own user name; the server ignores it.
        let encoded = STANDARD.encode(format!("jj-xet:{password}"));
        request = request.header(AUTHORIZATION, format!("{BASIC} {encoded}"));
    }
    match spec.get("json") {
        Some(json) => request
            .header(CONTENT_TYPE, LFS_JSON)
            .body(Body::from(substitute(json, values).to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap()
}

#[tokio::test]
async fn jj_xet_mock_expectations() {
    let expectations: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../compat/fixtures/lfs/jj-xet-expectations.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/m").await;
    let size = a_bin_size(&hub, &repo_id(&hub, "alice", "m").await).await;
    let values = [
        ("{oid}", Value::from(A_BIN_SHA256)),
        ("{size}", Value::from(size)),
        ("{unknown_oid}", Value::from(UNKNOWN_OID)),
        ("{xet_hash}", Value::from(A_BIN_HASH)),
    ];
    let cases = expectations["cases"].as_array().unwrap();
    assert!(cases.len() >= 7, "the transcription lost cases");
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let reply = hub
            .send(jj_request(case, "/alice/m.git/info/lfs", &values))
            .await;
        let expected = &case["response"];
        assert_eq!(
            u64::from(reply.status.as_u16()),
            expected["status"].as_u64().unwrap(),
            "{name}: {}",
            reply.text()
        );
        assert!(
            !reply.text().contains(JJ_WRONG),
            "{name} echoes the credential"
        );
        let body = reply.json();
        for (dotted, want) in expected["json"].as_object().into_iter().flatten() {
            let got = at(&body, dotted);
            let want = substitute(want, &values);
            assert!(matches(got, &want), "{name}: {dotted} is {got}, not {want}");
        }
    }
}

#[tokio::test]
async fn pointer_sha256_unknown_lists_without_xet_hash() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let unknown: [u8; 32] =
        core::array::from_fn(|i| u8::from_str_radix(&UNKNOWN_OID[2 * i..2 * i + 2], 16).unwrap());
    // A pointer pushed without its object, as a Git client may commit one.
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("a pointer without its object"),
            file("p.bin", &openxet_git::canonical_pointer(&unknown, 12_345)),
        ],
    )
    .await;
    let tree = hub
        .call(
            Method::GET,
            "/api/models/alice/m/tree/main",
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(tree.status, StatusCode::OK, "{}", tree.text());
    let entry = support::by_path(&tree.json(), "p.bin").clone();
    assert_eq!(entry["lfs"]["oid"], UNKNOWN_OID, "{entry}");
    assert_eq!(entry["size"], 12_345);
    assert!(entry.get("xetHash").is_none(), "{entry}");
    // And LFS answers it as an object the repository doesn't have.
    let reply = batch(
        &hub,
        "alice/m",
        Auth::Basic(ALICE),
        "download",
        WITH_XET,
        &[(UNKNOWN_OID, 12_345)],
    )
    .await;
    assert_eq!(reply.json()["objects"][0]["error"]["code"], 404);
}
