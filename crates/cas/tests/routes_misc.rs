//! The CAS router's cross-cutting behaviour: the error body, request ids,
//! bearer authentication, unserved routes and telemetry (S10 "Conventions
//! for every route", "Status codes and retries"; design §5.9).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;

use common::{Access, ROUTES, Reply, SCOPE_A, TestServer, UNSERVED};
use openxet_auth::Mode;
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, WWW_AUTHENTICATE};

/// The telemetry body limit: 64 KiB.
const TELEMETRY_BODY_LIMIT: usize = 64 * 1024;

/// The `Authorization` value for `token`. The scheme is a constant so the
/// repository's secret scanner, which flags any literal bearer value, can
/// tell test tokens from leaks.
fn bearer(token: &str) -> String {
    const SCHEME: &str = "Bearer";
    format!("{SCHEME} {token}")
}

#[tokio::test]
async fn telemetry_is_accepted_and_dropped() {
    let server = TestServer::start_default().await;
    let marker = format!("telemetry-marker-{}", uuid::Uuid::new_v4().simple());
    let body = format!(
        r#"{{"time":"2026-10-23T00:00:00.000Z","event":"xet_upload_summary","session_id":"{marker}","user_agent":"hf_xet/1.7.0","metrics":{{"marker":"{marker}"}}}}"#
    );

    let reply = Reply::of(
        server
            .request(Method::POST, "/v1/telemetry")
            .header(AUTHORIZATION, bearer(&server.read_token))
            .header("content-type", "application/json")
            .body(body),
    )
    .await;

    assert_eq!(reply.status, 204);
    assert!(reply.body.is_empty());
    let logs = server.logs();
    assert!(
        logs.contains("/v1/telemetry"),
        "the capture must see the request: {logs}"
    );
    assert!(!logs.contains(&marker), "the body was logged: {logs}");
}

#[tokio::test]
async fn telemetry_over_the_body_limit_is_413() {
    let server = TestServer::start_default().await;

    let send = |len: usize| {
        Reply::of(
            server
                .request(Method::POST, "/v1/telemetry")
                .header(AUTHORIZATION, bearer(&server.read_token))
                .body(vec![b'x'; len]),
        )
    };

    assert_eq!(send(TELEMETRY_BODY_LIMIT).await.status, 204);
    send(TELEMETRY_BODY_LIMIT + 1)
        .await
        .assert_error(413, "payload_too_large");
}

#[tokio::test]
async fn xorb_grants_answer_404() {
    let server = TestServer::start_default().await;
    let path =
        "/v1/xorb-grants/default/0000000000000000000000000000000000000000000000000000000000000000";

    for token in [&server.write_token, &server.read_token] {
        Reply::of(
            server
                .request(Method::POST, path)
                .header(AUTHORIZATION, bearer(token)),
        )
        .await
        .assert_error(404, "not_found");
    }
}

#[tokio::test]
async fn unknown_route_is_404_with_error_schema() {
    let server = TestServer::start_default().await;

    for (method, path) in UNSERVED {
        for auth in [None, Some(&server.write_token)] {
            let mut request = server.request(method.clone(), path);
            if let Some(token) = auth {
                request = request.header(AUTHORIZATION, bearer(token));
            }
            Reply::of(request).await.assert_error(404, "not_found");
        }
    }
    for method in [Method::PUT, Method::DELETE, Method::PATCH, Method::OPTIONS] {
        Reply::of(server.request(method, "/v1/nothing"))
            .await
            .assert_error(404, "not_found");
    }
    let head = Reply::of(server.request(Method::HEAD, "/v1/nothing")).await;
    assert_eq!(head.status, 404);
}

#[tokio::test]
async fn file_chunk_hashes_answer_404() {
    let server = TestServer::start_default().await;

    Reply::of(
        server
            .request(
                Method::GET,
                "/v2/file-chunk-hashes/0000000000000000000000000000000000000000000000000000000000000000",
            )
            .header(AUTHORIZATION, bearer(&server.read_token)),
    )
    .await
    .assert_error(404, "not_found");
}

#[tokio::test]
async fn known_path_with_another_method_is_405_with_error_schema() {
    let server = TestServer::start_default().await;

    let reply = Reply::of(
        server
            .request(Method::GET, "/v1/telemetry")
            .header(AUTHORIZATION, bearer(&server.read_token)),
    )
    .await;

    reply.assert_error(405, "method_not_allowed");
    assert_eq!(reply.headers["allow"], "POST");
}

#[tokio::test]
async fn every_response_has_a_request_id() {
    let server = TestServer::start_default().await;
    let mut requests = Vec::new();
    for route in ROUTES {
        requests.push(
            server
                .request(route.method.clone(), route.path)
                .header(AUTHORIZATION, bearer(&server.write_token)),
        );
        requests.push(server.request(route.method.clone(), route.path));
    }
    for (method, path) in UNSERVED {
        requests.push(server.request(method.clone(), path));
    }
    requests.push(server.request(Method::GET, "/v1/telemetry"));

    let mut seen = HashSet::new();
    for request in requests {
        let reply = Reply::of(request).await;
        let id = reply
            .headers
            .get("x-request-id")
            .unwrap_or_else(|| panic!("no X-Request-Id on a {} reply", reply.status))
            .to_str()
            .unwrap()
            .to_owned();
        let uuid = uuid::Uuid::parse_str(&id).unwrap();
        assert_eq!(uuid.get_version_num(), 7, "{id} is not a UUIDv7");
        assert_eq!(id, uuid.hyphenated().to_string(), "not canonical: {id}");
        assert!(
            server.logs().contains(&format!("request_id={id}")),
            "request id {id} is not a span field in the logs"
        );
        assert!(seen.insert(id), "request ids repeat");
    }
}

#[tokio::test]
async fn a_client_supplied_request_id_is_replaced() {
    let server = TestServer::start_default().await;

    let reply = Reply::of(
        server
            .request(Method::GET, "/no/such/route")
            .header("x-request-id", "forged\u{2028}id"),
    )
    .await;

    let ids: Vec<_> = reply.headers.get_all("x-request-id").iter().collect();
    assert_eq!(ids.len(), 1);
    assert_ne!(ids[0], "forged\u{2028}id");
    assert!(!server.logs().contains("forged"));
}

#[tokio::test]
async fn missing_or_bad_credential_is_401_and_wrong_mode_is_403() {
    let server = TestServer::start_default().await;
    let now = openxet_cas::Clock::now(&server.clock);
    let expiring = server
        .signer
        .sign("alice", SCOPE_A, Mode::Write, now + 60, now)
        .unwrap();

    for route in ROUTES {
        let send = |auth: Option<String>| {
            let mut request = server.request(route.method.clone(), route.path);
            if let Some(auth) = auth {
                request = request.header(AUTHORIZATION, auth);
            }
            Reply::of(request)
        };

        let missing = send(None).await;
        missing.assert_error(401, "missing_credential");
        assert_eq!(missing.headers[WWW_AUTHENTICATE], "Bearer");

        let invalid = send(Some(bearer("oxt_test_unknown"))).await;
        invalid.assert_error(401, "invalid_credential");
        assert_eq!(
            invalid.headers[WWW_AUTHENTICATE],
            r#"Bearer error="invalid_token""#
        );
        send(Some(format!("Basic {}", server.write_token)))
            .await
            .assert_error(401, "invalid_credential");
        send(Some(server.write_token.clone()))
            .await
            .assert_error(401, "invalid_credential");
        let read = send(Some(bearer(&server.read_token))).await;
        if route.access == Access::Write {
            read.assert_error(403, "insufficient_scope");
        } else {
            assert!(read.status.is_success(), "{} {}", route.path, read.body);
        }

        let accepted = send(Some(bearer(&expiring))).await;
        assert!(
            accepted.status.is_success(),
            "{} {}",
            route.path,
            accepted.body
        );
        let lowercase = send(Some(format!("bearer {}", server.write_token))).await;
        assert!(lowercase.status.is_success(), "{}", lowercase.body);
    }

    server.clock.advance(60);
    for route in ROUTES {
        Reply::of(
            server
                .request(route.method.clone(), route.path)
                .header(AUTHORIZATION, bearer(&expiring)),
        )
        .await
        .assert_error(401, "expired_credential");
    }
}

#[tokio::test]
async fn responses_never_echo_credential() {
    let server = TestServer::start_default().await;
    let now = openxet_cas::Clock::now(&server.clock);
    let jwt = server
        .signer
        .sign("alice", SCOPE_A, Mode::Read, now + 600, now)
        .unwrap();
    let unknown = format!("oxt_test_{}", uuid::Uuid::new_v4().simple());
    let tokens = [
        server.write_token.clone(),
        server.read_token.clone(),
        server.other_scope_token.clone(),
        jwt,
        unknown,
    ];
    let mut targets: Vec<(Method, &str)> =
        ROUTES.iter().map(|r| (r.method.clone(), r.path)).collect();
    targets.extend(UNSERVED.iter().cloned());
    targets.push((Method::GET, "/v1/telemetry"));

    for (method, path) in &targets {
        for token in &tokens {
            for auth in [bearer(token), format!("Basic {token}"), token.clone()] {
                let reply = Reply::of(
                    server
                        .request(method.clone(), path)
                        .header(AUTHORIZATION, auth),
                )
                .await;
                assert!(!reply.body.contains(token.as_str()), "{method} {path} body");
                for (name, value) in &reply.headers {
                    assert!(
                        !String::from_utf8_lossy(value.as_bytes()).contains(token.as_str()),
                        "{method} {path} header {name}"
                    );
                }
            }
        }
        let reply = Reply::of(server.request(method.clone(), path)).await;
        for token in &tokens {
            assert!(!reply.body.contains(token.as_str()));
        }
    }
    // A credential in a path segment or the query must not reach the logs
    // either: spans record the route template, never the raw path.
    for token in &tokens {
        for (method, path) in [
            (Method::GET, format!("/{token}")),
            (Method::HEAD, format!("/v1/xorbs/default/{token}")),
            (Method::POST, format!("/v1/telemetry/{token}")),
            (Method::POST, format!("/v1/telemetry?sig={token}")),
        ] {
            let reply = Reply::of(
                server
                    .request(method, &path)
                    .header(AUTHORIZATION, bearer(&server.read_token)),
            )
            .await;
            assert!(!reply.body.contains(token.as_str()), "{path} body");
        }
    }
    let logs = server.logs();
    for token in &tokens {
        assert!(!logs.contains(token.as_str()), "a credential was logged");
    }
}

#[tokio::test]
async fn spans_record_the_route_template() {
    let server = TestServer::start_default().await;

    Reply::of(
        server
            .request(Method::POST, "/v1/telemetry")
            .header(AUTHORIZATION, bearer(&server.read_token)),
    )
    .await;
    Reply::of(server.request(Method::GET, "/v1/some/raw/path")).await;

    let logs = server.logs();
    assert!(logs.contains("route=/v1/telemetry"), "{logs}");
    assert!(logs.contains("route=unmatched"), "{logs}");
    assert!(!logs.contains("/v1/some/raw/path"), "{logs}");
}

#[tokio::test]
async fn error_hint_present() {
    let server = TestServer::start_default().await;
    let route = &ROUTES[0];

    let replies = [
        Reply::of(server.request(Method::GET, "/no/such/route")).await,
        Reply::of(server.request(Method::GET, "/v1/telemetry")).await,
        Reply::of(server.request(route.method.clone(), route.path)).await,
        Reply::of(
            server
                .request(route.method.clone(), route.path)
                .header(AUTHORIZATION, bearer("oxt_test_unknown")),
        )
        .await,
    ];

    for reply in replies {
        let json = reply.json();
        let hint = json["hint"].as_str().unwrap();
        assert!(!hint.trim().is_empty(), "empty hint: {}", reply.body);
        assert_ne!(
            json["hint"], json["message"],
            "the hint repeats the message"
        );
    }
}
