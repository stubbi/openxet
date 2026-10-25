//! The CAS router's cross-cutting behaviour: the error body, request ids,
//! bearer authentication, unserved routes and telemetry (S10 "Conventions
//! for every route", "Status codes and retries"; design §5.9).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::sync::Arc;

use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::Request;
use common::xorb::HF_XET_XORB;
use common::{
    Access, FILE_CHUNK_HASHES_PATH, FailingBucket, PROBE_BODIES, ROUTES, Reply, Route, SCOPE_A,
    TRANSFER_PATH, TestServer, UNAUTHENTICATED, UNSERVED, XORB_GRANT_PATH, bearer, config,
};
use openxet_auth::Mode;
use openxet_cas::{BodyDeadline, CasConfig, TELEMETRY_BODY_LIMIT, starting_router};
use openxet_index::Visibility;
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use tower::ServiceExt as _;

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
async fn telemetry_with_an_oversized_content_length_is_413_before_reading() {
    let server = TestServer::start_default().await;
    let head = format!(
        "POST /v1/telemetry HTTP/1.1\r\nAuthorization: {}\r\nContent-Length: {}",
        bearer(&server.read_token),
        TELEMETRY_BODY_LIMIT + 1
    );

    // No body is sent: answering proves the server didn't wait to read it.
    let answer = server.raw(&head, b"", false).await;

    assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");
    assert!(answer.contains("\"payload_too_large\""), "{answer}");
}

#[tokio::test]
async fn an_early_413_closes_the_connection_so_the_next_request_succeeds() {
    let server = TestServer::start_default().await;
    let telemetry = |body: Vec<u8>| {
        server
            .request(Method::POST, "/v1/telemetry")
            .header(AUTHORIZATION, bearer(&server.read_token))
            .body(body)
    };

    // One pooled client: the 413 is sent before the body is read, so the
    // connection must not be reused with the unread bytes still on it.
    let refused = Reply::of(telemetry(vec![b'{'; TELEMETRY_BODY_LIMIT + 1])).await;
    refused.assert_error(413, "payload_too_large");
    assert_eq!(refused.headers["connection"], "close");
    let next = Reply::of(telemetry(b"{}".to_vec())).await;
    assert_eq!(next.status, 204, "{}", next.body);
}

#[tokio::test]
async fn telemetry_with_a_truncated_body_is_400() {
    let server = TestServer::start_default().await;
    let head = format!(
        "POST /v1/telemetry HTTP/1.1\r\nAuthorization: {}\r\nContent-Length: 100",
        bearer(&server.read_token)
    );

    let answer = server.raw(&head, b"{\"time\":", true).await;

    assert!(answer.starts_with("HTTP/1.1 400"), "{answer}");
    assert!(answer.contains("\"bad_request\""), "{answer}");
}

#[tokio::test]
async fn xorb_grants_answer_404() {
    let server = TestServer::start_default().await;
    let path = XORB_GRANT_PATH;

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
            .request(Method::GET, FILE_CHUNK_HASHES_PATH)
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
    requests.push(server.request(Method::GET, TRANSFER_PATH));

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
            route.assert_authorized(&read);
        }

        route.assert_authorized(&send(Some(bearer(&expiring))).await);
        route.assert_authorized(&send(Some(format!("bearer {}", server.write_token))).await);
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
        server.other_scope_read_token.clone(),
        jwt,
        unknown,
    ];
    let mut targets: Vec<(Method, &str)> =
        ROUTES.iter().map(|r| (r.method.clone(), r.path)).collect();
    targets.extend(UNSERVED.iter().cloned());
    targets.push((Method::GET, "/v1/telemetry"));
    // The transfer route reads no `Authorization`, but must not echo one.
    targets.push((Method::GET, TRANSFER_PATH));

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

#[tokio::test]
async fn probes_need_no_credential_and_name_no_data() {
    let server = TestServer::start_default().await;
    for (path, body) in UNAUTHENTICATED.iter().zip(PROBE_BODIES) {
        for method in [Method::GET, Method::HEAD] {
            let reply = Reply::of(server.request(method.clone(), path)).await;
            assert_eq!(reply.status, 200, "{method} {path}");
            assert!(reply.headers.contains_key("x-request-id"));
            if method == Method::GET {
                assert_eq!(reply.body, *body, "{path}");
            }
        }
        // A credential changes nothing, valid or not.
        let reply = Reply::of(
            server
                .request(Method::GET, path)
                .header(AUTHORIZATION, bearer("oxt_not-a-token")),
        )
        .await;
        assert_eq!(reply.status, 200);
    }
}

async fn starting(path: &str) -> (u16, Option<String>, String) {
    let response = starting_router()
        .oneshot(Request::get(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status().as_u16();
    let retry = response
        .headers()
        .get("retry-after")
        .map(|v| v.to_str().unwrap().to_owned());
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, retry, String::from_utf8(body.to_vec()).unwrap())
}

#[tokio::test]
async fn a_starting_server_is_live_but_not_ready() {
    let (status, _, body) = starting("/healthz").await;
    assert_eq!((status, body.as_str()), (200, PROBE_BODIES[0]));

    for path in [
        "/readyz",
        "/v1/files/0000000000000000000000000000000000000000000000000000000000000000",
    ] {
        let (status, retry, body) = starting(path).await;
        assert_eq!(status, 503, "{path}");
        assert_eq!(retry.as_deref(), Some("1"), "{path}");
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["schema"], "openxet.error.v1");
        assert_eq!(json["code"], "not_ready");
    }
}

#[tokio::test]
async fn a_ready_server_probes_its_index_and_bucket() {
    let server = TestServer::start_default().await;
    let reply = Reply::of(server.request(Method::GET, "/readyz")).await;
    assert_eq!(
        (reply.status.as_u16(), reply.body.as_str()),
        (200, PROBE_BODIES[1])
    );
}

#[tokio::test]
async fn a_bucket_that_cannot_take_writes_is_not_ready() {
    // Issue #49: a full disk reads as not ready, naming the component and
    // not the cause, which is logged.
    let server = TestServer::start_with(config(Visibility::Global), Arc::new(FailingBucket)).await;
    for _ in 0..2 {
        let reply = Reply::of(server.request(Method::GET, "/readyz")).await;
        reply.assert_error(503, "unavailable");
        let message = reply.json()["message"].as_str().unwrap().to_owned();
        assert!(message.contains("storage"), "{message}");
        assert!(!message.contains("failing-bucket"), "{message}");
        assert!(!message.contains("no space"), "{message}");
    }
    let reply = Reply::of(server.request(Method::GET, "/healthz")).await;
    assert_eq!(reply.status.as_u16(), 200);
}

#[tokio::test]
async fn an_idle_upload_body_is_cut_off_with_408() {
    let cfg = CasConfig {
        body_deadline: BodyDeadline::new(Duration::from_millis(300), 1).unwrap(),
        send_deadline: openxet_cas::SendDeadline::DEFAULT,
        memory_budget: openxet_cas::MemoryBudget::DEFAULT_BYTES,
        ..config(Visibility::Global)
    };
    let server = TestServer::start(cfg).await;
    let head = format!(
        "POST /v1/xorbs/default/{HF_XET_XORB} HTTP/1.1\r\nAuthorization: {}\r\nContent-Length: 1000",
        bearer(&server.write_token)
    );

    let started = Instant::now();
    let answer = server.raw(&head, &[0; 10], false).await;
    assert!(answer.starts_with("HTTP/1.1 408"), "{answer}");
    assert!(answer.contains("\"code\":\"body_timeout\""), "{answer}");
    assert!(
        answer.to_ascii_lowercase().contains("connection: close"),
        "{answer}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn a_refused_body_drain_is_bounded_by_the_deadline() {
    // Ruling M1-R22 drains a refused body; the deadline keeps a client that
    // stops sending from holding the connection.
    let cfg = CasConfig {
        body_deadline: BodyDeadline::new(Duration::from_millis(300), 1).unwrap(),
        send_deadline: openxet_cas::SendDeadline::DEFAULT,
        memory_budget: openxet_cas::MemoryBudget::DEFAULT_BYTES,
        ..config(Visibility::Global)
    };
    let server = TestServer::start(cfg).await;
    let head = format!("POST /v1/xorbs/default/{HF_XET_XORB} HTTP/1.1\r\nContent-Length: 1000");

    let answer = server.raw(&head, &[0; 10], false).await;
    assert!(answer.starts_with("HTTP/1.1 401"), "{answer}");
}

#[test]
fn body_deadline_rejects_zero() {
    assert!(BodyDeadline::new(Duration::ZERO, 1).is_none());
    assert!(BodyDeadline::new(Duration::from_secs(1), 0).is_none());
    assert!(BodyDeadline::new(Duration::from_secs(1), 1).is_some());
}

/// Whether the concrete `path` (its query ignored) is an instance of the
/// route `template`, whose `{name}` segments match any one segment.
fn instance_of(template: &str, path: &str) -> bool {
    let path = path.split('?').next().unwrap();
    let (mut t, mut p) = (template.split('/'), path.split('/'));
    loop {
        match (t.next(), p.next()) {
            (None, None) => return true,
            (Some(t), Some(p)) if t == p || (t.starts_with('{') && !p.is_empty()) => {}
            _ => return false,
        }
    }
}

/// The template among `templates` that serves the concrete `path`, as the
/// router picks it: at each segment a literal wins over a parameter.
fn serving<'a>(templates: impl Iterator<Item = &'a str>, path: &str) -> Option<&'a str> {
    let kinds = |t: &str| -> Vec<bool> { t.split('/').map(|s| s.starts_with('{')).collect() };
    templates
        .filter(|t| instance_of(t, path))
        .min_by_key(|t| kinds(t))
}

/// A concrete path for `template`: the `default` prefix and a hash no test
/// stores.
fn concrete(template: &str) -> String {
    template
        .replace("{prefix}", "default")
        .replace("{hash}", &"0".repeat(64))
}

/// The endpoints served without an `Authorization` credential, by exact
/// template: the probes, and the signed transfer route, whose signature is
/// its credential (ADR 0002).
const ALLOWLIST: &[(Method, &str)] = &[
    (Method::GET, "/healthz"),
    (Method::GET, "/readyz"),
    (Method::GET, "/v1/transfer/xorbs/{hash}"),
    (Method::HEAD, "/v1/transfer/xorbs/{hash}"),
];

#[test]
fn every_served_route_is_in_routes_or_the_allowlist() {
    // A route missing from `ROUTES` escapes the authentication, scoping and
    // echo matrices (design §5.9 reqs 1, 2 and 6), so every endpoint the
    // router serves is either there or on the explicit allowlist. A
    // `ROUTES` entry covers the one template that serves its path, so a
    // broader template that would also match it is still uncovered.
    let endpoints = openxet_cas::endpoints();
    let serving_route = |route: &Route| {
        serving(
            endpoints
                .iter()
                .filter(|e| e.method == route.method)
                .map(|e| e.path),
            route.path,
        )
    };
    for endpoint in &endpoints {
        let (method, template) = (&endpoint.method, endpoint.path);
        let in_routes = ROUTES
            .iter()
            .any(|r| r.method == *method && serving_route(r) == Some(template));
        let allowed = ALLOWLIST.iter().any(|(m, t)| m == method && *t == template);
        assert!(
            in_routes || allowed,
            "{method} {template} is served but not in common::ROUTES; add it there so the \
             credential and echo matrices cover it"
        );
    }
    for route in ROUTES {
        assert!(
            serving_route(route).is_some(),
            "common::ROUTES lists {} {}, which the router doesn't serve",
            route.method,
            route.path
        );
    }
    for (method, template) in ALLOWLIST {
        assert!(
            endpoints
                .iter()
                .any(|e| e.method == *method && e.path == *template),
            "the allowlist names {method} {template}, which the router doesn't serve"
        );
    }
}

#[tokio::test]
async fn router_serves_exactly_its_endpoints() {
    // `endpoints()` comes from the calls that build the router. A `405`
    // lists in `Allow` every method a path really serves, so probing each
    // path with a method no route uses checks the list against what is
    // served, including the `HEAD` a `GET` handler answers.
    let server = TestServer::start_default().await;
    let endpoints = openxet_cas::endpoints();
    let paths: HashSet<&str> = endpoints.iter().map(|e| e.path).collect();
    for template in paths {
        let mut stated: HashSet<String> = endpoints
            .iter()
            .filter(|e| e.path == template)
            .map(|e| e.method.to_string())
            .collect();
        if stated.contains("GET") {
            stated.insert("HEAD".to_owned());
        }
        let reply = Reply::of(server.request(Method::PATCH, &concrete(template))).await;
        reply.assert_error(405, "method_not_allowed");
        let allowed: HashSet<String> = reply.headers["allow"]
            .to_str()
            .unwrap()
            .split(',')
            .map(|m| m.trim().to_owned())
            .collect();
        assert_eq!(allowed, stated, "{template}");
    }
}
