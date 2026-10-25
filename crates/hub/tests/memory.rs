//! The Hub's reads draw on the memory budget the CAS shares (ruling
//! M1-R40): the file gateway reserves its plan until its body is sent,
//! dropped or past its send deadline, and `resolve` reserves a Git blob
//! before inflating it.

#![allow(clippy::unwrap_used)]

mod support;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt as _;
use openxet_cas::{MemoryBudget, SendDeadline};
use openxet_hub::SharedMemory;
use openxet_hub::test_support::{BEARER, TestHub};
use support::files::{Stored, bearer, data, request, store_file};
use support::{ALICE, commit_ok, file, header, hub, repo_id, with_cas};
use tower::ServiceExt as _;

/// A Hub with `alice/m` drawing on `budget` with `deadline`, its router,
/// and a file of three 64 KiB chunks stored there.
async fn with_budget(budget: MemoryBudget, deadline: SendDeadline) -> (TestHub, Router, Stored) {
    let TestHub { state, dir, .. } = hub().await;
    let memory = SharedMemory {
        budget,
        send_deadline: deadline,
    };
    let hub = TestHub::with_state(dir, state.with_memory(memory));
    hub.create(ALICE, "alice", "m", None).await;
    let id = repo_id(&hub, "alice", "m").await;
    let chunks = (0..3u8).map(|c| data(c, 64 << 10)).collect();
    let stored = store_file(&hub, &id, &[chunks], &[(0, 0, 3)]).await;
    let router = with_cas(&hub);
    (hub, router, stored)
}

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
    let (hub, router, stored) = with_budget(budget.clone(), SendDeadline::DEFAULT).await;
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
async fn gateway_body_ends_at_its_send_deadline() {
    // A client that stops reading holds the plan for at most the send
    // deadline: the timer releases it, and the body then fails.
    let budget = MemoryBudget::new(MemoryBudget::DEFAULT_BYTES);
    let deadline = SendDeadline::new(Duration::from_millis(50), u64::MAX).unwrap();
    let (hub, router, stored) = with_budget(budget.clone(), deadline).await;
    let id = repo_id(&hub, "alice", "m").await;
    let url = format!("/v1/transfer/files/{id}/{}", stored.hash);
    let full = budget.available();
    let mut body = open(&router, &url).await.into_body();
    assert!(budget.available() < full);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(budget.available(), full, "released at the deadline");
    assert!(body.frame().await.unwrap().is_err(), "the body then fails");
}

#[tokio::test]
async fn gateway_reserves_its_plan() {
    // A budget whose free bytes can't hold the plan: `503 memory_busy`
    // with `Retry-After` once the answer wait passes, then served when
    // the bytes come back.
    let budget = MemoryBudget::new(MemoryBudget::MIN_BYTES)
        .with_waits(Duration::from_millis(10), Duration::from_millis(10));
    let (hub, router, stored) = with_budget(budget.clone(), SendDeadline::DEFAULT).await;
    let id = repo_id(&hub, "alice", "m").await;
    let url = format!("/v1/transfer/files/{id}/{}", stored.hash);
    let share = budget.bytes() / 8;
    let mut held = Vec::new();
    while budget.available() > 0 {
        let take = budget.available().min(share);
        held.push(budget.reserve_answer(take).await.unwrap());
    }
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
    let (hub, router, _) = with_budget(budget.clone(), SendDeadline::DEFAULT).await;
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
