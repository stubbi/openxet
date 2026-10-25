//! The Hub's CAS token routes, `GET /api/{type}s/{id}/xet-{read|write}-token/{rev}`,
//! against S3's huggingface.co fixtures (answer 2) and ruling M1-R11.

#![allow(clippy::unwrap_used)]

mod support;

use axum::http::{Method, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openxet_auth::{Credential, Mode};
use openxet_hub::test_support::{PUBLIC_URL, Reply, TestHub, assert_read_keys, fixture_json};
use serde_json::Value;
use support::{
    A_BIN_HASH, ALICE, BOB, CAROL, commit_ok, file, header, hub, repo_id, send, upload_capture,
    with_cas, xet_token,
};

async fn token(hub: &TestHub, user: &str, repo: &str, kind: &str, rev: &str) -> Reply {
    hub.call(
        Method::GET,
        &format!("/api/models/{repo}/xet-{kind}-token/{rev}"),
        Some(user),
        None,
    )
    .await
}

/// The claims of a CAS token.
fn claims(token: &str) -> Value {
    let payload = token.split('.').nth(1).unwrap();
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).unwrap()).unwrap()
}

#[tokio::test]
async fn token_in_body_and_headers() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    for (kind, fixture) in [
        (
            "write",
            "09-upload-folder--03-GET-models-xet-write-token.json",
        ),
        (
            "read",
            "27-hf-hub-download-big--02-GET-models-xet-read-token.json",
        ),
    ] {
        let reply = token(&hub, ALICE, "alice/m", kind, "main").await;
        assert_eq!(reply.status, StatusCode::OK, "{kind}: {}", reply.text());
        let body = reply.json();
        // `hf_xet` reads the body (`CasJWTInfo`), `huggingface_hub` the
        // headers (`utils/_xet.py` lines 95–97).
        assert_read_keys(
            &fixture_json(fixture),
            &body,
            &["accessToken", "exp", "casUrl"],
        );
        assert_eq!(body["casUrl"], PUBLIC_URL, "no trailing slash");
        assert_eq!(
            reply.header("x-xet-cas-url"),
            body["casUrl"].as_str(),
            "{kind}"
        );
        assert_eq!(
            reply.header("x-xet-access-token"),
            body["accessToken"].as_str()
        );
        assert_eq!(
            reply.header("x-xet-token-expiration").unwrap(),
            body["exp"].to_string()
        );
        assert_eq!(reply.header("cache-control"), Some("no-store"));
    }
}

#[tokio::test]
async fn write_token_needs_write_access() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    // Bob may read alice/*: a read token, but no write token.
    assert_eq!(
        token(&hub, BOB, "alice/m", "read", "main").await.status,
        StatusCode::OK
    );
    let refused = token(&hub, BOB, "alice/m", "write", "main").await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.text());
    assert_eq!(refused.json()["code"], "forbidden");
    // Carol may not read it: the unknown-repository answer for both.
    for kind in ["read", "write"] {
        let hidden = token(&hub, CAROL, "alice/m", kind, "main").await;
        let missing = token(&hub, CAROL, "alice/none", kind, "main").await;
        assert_eq!(hidden.status, StatusCode::NOT_FOUND);
        assert_eq!(hidden.comparable(), missing.comparable(), "{kind}");
    }
    // Without a credential: 401.
    let anonymous = hub
        .call(
            Method::GET,
            "/api/models/alice/m/xet-read-token/main",
            None,
            None,
        )
        .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_scope_is_repo_id_and_tenant_namespace() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let id = repo_id(&hub, "alice", "m").await;
    for (kind, mode) in [("read", Mode::Read), ("write", Mode::Write)] {
        let body = token(&hub, ALICE, "alice/m", kind, "main").await.json();
        let jwt = body["accessToken"].as_str().unwrap();
        let claims = claims(jwt);
        assert_eq!(claims["scope"], format!("repo:{id}"));
        assert_eq!(claims["tenant"], "alice");
        assert_eq!(claims["sub"], "alice");
        assert_eq!(claims["exp"], body["exp"]);
        // `hub.token_ttl`, 15 minutes by default.
        assert_eq!(
            claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
            900
        );
        // The CAS verifies it with the server's own key.
        let now = claims["iat"].as_u64().unwrap();
        match hub.state.auth.authenticate(jwt, now).unwrap() {
            Credential::Cas(grant) => {
                assert_eq!(grant.scope, format!("repo:{id}"));
                assert_eq!(grant.mode, mode);
            }
            other => panic!("not a CAS credential: {other:?}"),
        }
    }
}

#[tokio::test]
async fn write_token_refresh_ignores_revision() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let first = token(&hub, ALICE, "alice/m", "write", "main").await;
    assert_eq!(first.status, StatusCode::OK);
    let initial = hub
        .call(Method::GET, "/api/models/alice/m/refs", Some(ALICE), None)
        .await
        .json()["branches"][0]["targetCommit"]
        .as_str()
        .unwrap()
        .to_owned();
    // Another commit moves `main` while an upload is under way.
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[header("meanwhile"), file("x.txt", b"x")],
    )
    .await;
    // `hf_xet` refreshes at the branch name, which now names another
    // commit; a PR commit refreshes at the literal `None`. Every revision,
    // even one that names nothing, gets a token of the same scope.
    let scope = claims(first.json()["accessToken"].as_str().unwrap())["scope"].clone();
    for rev in ["main", "None", &initial, "no-such-branch"] {
        let reply = token(&hub, ALICE, "alice/m", "write", rev).await;
        assert_eq!(reply.status, StatusCode::OK, "{rev}: {}", reply.text());
        let refreshed = claims(reply.json()["accessToken"].as_str().unwrap());
        assert_eq!(refreshed["scope"], scope, "{rev}");
    }
}

#[tokio::test]
async fn read_token_lets_cas_reconstruct_only_this_repo() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "a", None).await;
    hub.create(ALICE, "alice", "b", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/a").await;
    let path = format!("/v2/reconstructions/{A_BIN_HASH}");
    let read_a = xet_token(&hub, ALICE, "alice/a", "read").await;
    let reply = send(&router, Method::GET, &path, &read_a, Vec::new()).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    // The file is registered to alice/a's scope only: alice/b's tokens,
    // read or write, don't reconstruct it.
    for kind in ["read", "write"] {
        let token_b = xet_token(&hub, ALICE, "alice/b", kind).await;
        let reply = send(&router, Method::GET, &path, &token_b, Vec::new()).await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{kind}: {}",
            reply.text()
        );
    }
}
