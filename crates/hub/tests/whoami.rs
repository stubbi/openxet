//! `GET /api/whoami-v2` against S3's fixture `01` (S3 lesson 2).

#![allow(clippy::unwrap_used)]

use axum::http::{Method, StatusCode};
use openxet_auth::{StaticToken, UserProfile};
use openxet_hub::test_support::{TestHub, assert_read_keys, fixture_json, user_token};

const ALICE: &str = "oxt_test_alice";
const NAMED: &str = "oxt_test_named";

#[tokio::test]
async fn whoami_has_every_field_the_client_reads() {
    let named = StaticToken {
        user: Some(UserProfile {
            name: Some("Bob Example".to_owned()),
            ..UserProfile::default()
        }),
        ..user_token(NAMED, "bob", &[])
    };
    let hub = TestHub::start(
        vec![user_token(ALICE, "alice", &["acme", "labs"]), named],
        vec![],
    )
    .await;
    let reply = hub
        .call(Method::GET, "/api/whoami-v2", Some(ALICE), None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    let expected = fixture_json("01-login--01-GET-whoami-v2.json");
    assert_read_keys(
        &expected,
        &body,
        &[
            "name",
            "orgs[].name",
            "auth.accessToken.displayName",
            "auth.accessToken.role",
            "id",
            "type",
            "fullname",
            "avatarUrl",
            "isPro",
            "auth.type",
        ],
    );
    assert_eq!(body["name"], "alice");
    assert_eq!(body["type"], "user");
    assert_eq!(body["orgs"][0]["name"], "acme");
    assert_eq!(body["orgs"][1]["name"], "labs");
    assert_eq!(body["id"].as_str().unwrap().len(), 24);
    assert_eq!(
        body["auth"]["accessToken"]["displayName"],
        "openxet-127.0.0.1"
    );

    // A token's `name` answers `name`; without an `orgs` claim, `orgs` is
    // still a list, which `hf auth whoami` iterates (cli/auth.py line 172).
    let body = hub
        .call(Method::GET, "/api/whoami-v2", Some(NAMED), None)
        .await
        .json();
    assert_eq!(body["name"], "Bob Example");
    assert_eq!(body["orgs"], serde_json::json!([]));

    // Without a credential: 401, with the challenge.
    let reply = hub.call(Method::GET, "/api/whoami-v2", None, None).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        reply.header("www-authenticate"),
        Some(r#"Bearer realm="openxet""#)
    );
    assert_eq!(
        reply.header("x-error-message"),
        Some(
            "Authentication required: pass an openxet access token as the Bearer token (hf auth login)"
        )
    );
    // With a bad one: the text `huggingface_hub` reports as an auth error.
    let reply = hub
        .call(
            Method::GET,
            "/api/whoami-v2",
            Some("oxt_test_unknown"),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        reply.header("x-error-message"),
        Some("Invalid credentials in Authorization header")
    );
}
