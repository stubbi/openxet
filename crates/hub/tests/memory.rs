//! The Hub's reads draw on the memory budget the CAS shares (ruling
//! M1-R40): the file gateway reserves its plan until its body is sent,
//! dropped or idle, and `resolve` reserves a Git blob
//! before inflating it.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt as _;
use openxet_cas::{MemoryBudget, SendDeadline};
use openxet_hub::test_support::{BEARER, TestHub};
use openxet_hub::{HubConfig, SharedMemory};
use support::files::{Stored, bearer, data, request, store_file};
use support::{ALICE, commit_ok, file, header, hub, repo_id, with_cas};
use tower::ServiceExt as _;

/// A Hub with `alice/m` drawing on `budget`, its gateway bodies going idle
/// after `idle`, its router,
/// and a file of three 64 KiB chunks stored there.
async fn with_budget(budget: MemoryBudget, idle: Duration) -> (TestHub, Router, Stored) {
    let TestHub { mut state, dir, .. } = hub().await;
    state.cfg.gateway_idle_timeout = idle;
    let memory = SharedMemory {
        budget,
        send_deadline: SendDeadline::DEFAULT,
    };
    let hub = TestHub::with_state(dir, state.with_memory(memory));
    hub.create(ALICE, "alice", "m", None).await;
    let id = repo_id(&hub, "alice", "m").await;
    let chunks = (0..3u8).map(|c| data(c, 64 << 10)).collect();
    let stored = store_file(&hub, &id, &[chunks], &[(0, 0, 3)]).await;
    let router = with_cas(&hub);
    (hub, router, stored)
}

/// The default idle timeout.
const IDLE: Duration = HubConfig::DEFAULT_GATEWAY_IDLE_TIMEOUT;

/// The gateway response for `url`, its body unread.
async fn open(router: &Router, url: &str) -> axum::response::Response {
    let request = Request::get(url)
        .header("authorization", format!("{BEARER} {ALICE}"))
        .body(Body::empty())
        .unwrap();
    router.clone().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn gateway_returns_its_reservation() {
    let budget = MemoryBudget::new(MemoryBudget::DEFAULT_BYTES);
    let (hub, router, stored) = with_budget(budget.clone(), IDLE).await;
    let id = repo_id(&hub, "alice", "m").await;
    let url = format!("/v1/transfer/files/{id}/{}", stored.hash);
    let full = budget.available();

    // Read whole: the reservation is back once the body has been sent.
    let response = open(&router, &url).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(budget.available() < full, "the plan is held while sending");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body == stored.content, "the bytes differ");
    assert_eq!(budget.available(), full);

    // Dropped after one piece: back too.
    let mut body = open(&router, &url).await.into_body();
    body.frame().await.unwrap().unwrap();
    assert!(budget.available() < full);
    drop(body);
    assert_eq!(budget.available(), full);
}

#[tokio::test]
async fn gateway_body_released_when_idle() {
    // `hub.gateway_idle_timeout` reaches the body: a client that stops
    // reading holds the transfer for at most that long, then the body
    // fails. The idle and steady cases are pinned on a paused clock in
    // `gateway::transfer`'s tests.
    let budget = MemoryBudget::new(MemoryBudget::DEFAULT_BYTES);
    let (hub, router, stored) = with_budget(budget.clone(), Duration::from_millis(100)).await;
    let id = repo_id(&hub, "alice", "m").await;
    let url = format!("/v1/transfer/files/{id}/{}", stored.hash);
    let full = budget.available();
    let mut body = open(&router, &url).await.into_body();
    assert!(budget.available() < full);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(budget.available(), full, "released once idle");
    assert!(body.frame().await.unwrap().is_err(), "the body then fails");
}

#[tokio::test]
async fn gateway_reserves_its_plan() {
    // A budget whose free bytes can't hold the plan: `503 memory_busy`
    // with `Retry-After` once the answer wait passes, then served when
    // the bytes come back.
    let budget = MemoryBudget::new(MemoryBudget::MIN_BYTES)
        .with_waits(Duration::from_millis(10), Duration::from_millis(10));
    let (hub, router, stored) = with_budget(budget.clone(), IDLE).await;
    let id = repo_id(&hub, "alice", "m").await;
    let url = format!("/v1/transfer/files/{id}/{}", stored.hash);
    let held = fill(&budget).await;
    let busy = request(&router, Method::GET, &url, &[bearer(ALICE)]).await;
    assert_eq!(
        busy.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        busy.text()
    );
    assert_eq!(busy.json()["code"], "memory_busy");
    assert_eq!(busy.header("retry-after"), Some("1"));
    drop(held);
    let served = request(&router, Method::GET, &url, &[bearer(ALICE)]).await;
    assert_eq!(served.status, StatusCode::OK);
}

#[tokio::test]
async fn resolve_reserves_a_git_blob_before_reading_it() {
    // A plain blob larger than one answer's share isn't inflated: `503`
    // with a hint to store it with Xet. One that fits holds its
    // reservation until sent.
    let budget = MemoryBudget::new(64 << 10);
    let (hub, router, _) = with_budget(budget.clone(), IDLE).await;
    let large = data(9, 20 << 10);
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("blobs"),
            file("large.txt", &large),
            file("small.txt", b"small\n"),
        ],
    )
    .await;
    let reply = request(
        &router,
        Method::GET,
        "/alice/m/resolve/main/large.txt",
        &[bearer(ALICE)],
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        reply.text()
    );
    assert_eq!(reply.json()["code"], "blob_too_large");
    assert!(
        reply.json()["hint"]
            .as_str()
            .unwrap()
            .contains("git lfs track")
    );
    assert_eq!(reply.header("retry-after"), None);

    let full = budget.available();
    let response = open(&router, "/alice/m/resolve/main/small.txt").await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(budget.available() < full);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"small\n");
    assert_eq!(budget.available(), full);
}

/// Reserves every free byte of `budget`, an answer's share at a time.
async fn fill(budget: &MemoryBudget) -> Vec<openxet_cas::Reservation> {
    let share = budget.answer_limit();
    let mut held = Vec::new();
    while budget.available() > 0 {
        let take = budget.available().min(share);
        held.push(budget.reserve_answer(take).await.unwrap());
    }
    held
}

#[tokio::test]
async fn preupload_reserves_gitattributes_before_reading_it() {
    // R-2: the target revision's `.gitattributes` is read within the
    // budget; a full budget answers `503 memory_busy`, and the read goes
    // through once the bytes come back. Git's 100 MiB cap is pinned in
    // `preupload`'s unit tests.
    let budget = MemoryBudget::new(MemoryBudget::MIN_BYTES)
        .with_waits(Duration::from_millis(10), Duration::from_millis(10));
    let (hub, _, _) = with_budget(budget.clone(), IDLE).await;
    let body = serde_json::json!({"files": [{"path": "w.safetensors", "size": 10, "sample": ""}]});
    let ask = || {
        hub.call(
            Method::POST,
            "/api/models/alice/m/preupload/main",
            Some(ALICE),
            Some(&body),
        )
    };
    let held = fill(&budget).await;
    let busy = ask().await;
    assert_eq!(
        busy.status,
        StatusCode::SERVICE_UNAVAILABLE,
        "{}",
        busy.text()
    );
    assert_eq!(busy.json()["code"], "memory_busy");
    assert_eq!(busy.header("retry-after"), Some("1"));
    drop(held);
    let read = ask().await;
    assert_eq!(read.status, StatusCode::OK, "{}", read.text());
    // The default `.gitattributes` routes `*.safetensors` to Xet.
    assert_eq!(read.json()["files"][0]["uploadMode"], "lfs");
    assert_eq!(budget.available(), budget.bytes());
}

/// A request (method, URL, extra headers) and the status it answers.
type Case<'a> = (Method, &'a str, Vec<(&'a str, String)>, StatusCode);

#[tokio::test]
async fn every_answer_returns_its_reservation() {
    // m-6: the release paths besides a whole read: resolve's `HEAD` and
    // `416`, the gateway's `416` and an `If-Range` that doesn't match.
    let budget = MemoryBudget::new(MemoryBudget::DEFAULT_BYTES);
    let (hub, router, stored) = with_budget(budget.clone(), IDLE).await;
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[header("small"), file("small.txt", b"small\n")],
    )
    .await;
    let id = repo_id(&hub, "alice", "m").await;
    let gateway = format!("/v1/transfer/files/{id}/{}", stored.hash);
    let size = stored.content.len();
    let cases: [Case<'_>; 4] = [
        (
            Method::HEAD,
            "/alice/m/resolve/main/small.txt",
            vec![],
            StatusCode::OK,
        ),
        (
            Method::GET,
            "/alice/m/resolve/main/small.txt",
            vec![("range", "bytes=99-".to_owned())],
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
        (
            Method::GET,
            &gateway,
            vec![("range", format!("bytes={size}-"))],
            StatusCode::RANGE_NOT_SATISFIABLE,
        ),
        (
            Method::GET,
            &gateway,
            vec![
                ("range", "bytes=0-9".to_owned()),
                ("if-range", "\"other\"".to_owned()),
            ],
            StatusCode::OK,
        ),
    ];
    for (method, url, extra, status) in cases {
        let mut headers = vec![bearer(ALICE)];
        headers.extend(extra);
        let reply = request(&router, method.clone(), url, &headers).await;
        assert_eq!(reply.status, status, "{method} {url}: {}", reply.text());
        assert_eq!(budget.available(), budget.bytes(), "{method} {url}");
    }
}
