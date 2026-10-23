//! The test harness itself: what other tests rely on it to capture.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::TestServer;

#[tokio::test]
async fn logs_from_blocking_threads_are_captured() {
    let server = TestServer::start_default().await;
    let marker = format!("blocking-marker-{}", uuid::Uuid::new_v4().simple());

    let logged = marker.clone();
    server
        .runtime
        .spawn_blocking(move || tracing::info!(marker = %logged, "from a blocking thread"))
        .await
        .unwrap();

    assert!(server.logs().contains(&marker), "{}", server.logs());
}
