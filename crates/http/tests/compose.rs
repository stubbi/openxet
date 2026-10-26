//! [`openxet_http::compose`]: parts with their own states, served behind
//! one fallback and one middleware stack (issue #46, ruling M2-R1).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Extension, State};
use axum::http::{Method, Request, StatusCode};
use axum::response::Response;
use openxet_http::{
    Area, BodyDeadline, Credential, Doc, Endpoint, Mounted, RequestId, Routes, X_REQUEST_ID,
    compose, on,
};
use tower::ServiceExt as _;

async fn greeting(State(greeting): State<Arc<str>>) -> String {
    greeting.to_string()
}

async fn count(State(count): State<u32>) -> String {
    count.to_string()
}

async fn echo_id(Extension(id): Extension<RequestId>) -> String {
    id.to_string()
}

async fn boom() -> &'static str {
    panic!("panic-payload-marker")
}

/// A part whose state is a string and one whose state is a number: each
/// crate of the server mounts its routes with its own state.
fn parts() -> Vec<Mounted> {
    let doc = || Doc::new(Area::Meta, "A test route", Credential::None);
    let words = Routes::new()
        .route("/words", on(Method::GET, doc(), greeting))
        .route("/words/id", on(Method::GET, doc(), echo_id))
        .with_state(Arc::<str>::from("hello"));
    let numbers = Routes::new()
        .route(
            "/numbers",
            on(Method::GET, doc(), count).on(Method::PUT, doc(), count),
        )
        .route("/numbers/panic", on(Method::GET, doc(), boom))
        .with_state(7_u32);
    vec![words, numbers]
}

fn app() -> Router {
    compose(parts(), BodyDeadline::DEFAULT)
}

async fn send(request: Request<Body>) -> (Response, String) {
    let response = app().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    let body = to_bytes(body, usize::MAX).await.unwrap();
    (
        Response::from_parts(parts, Body::empty()),
        String::from_utf8(body.to_vec()).unwrap(),
    )
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

fn assert_error(response: &Response, body: &str, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status, "{body}");
    assert_eq!(response.headers()["content-type"], "application/json");
    assert_eq!(response.headers().get_all(X_REQUEST_ID).iter().count(), 1);
    let json: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(json["schema"], "openxet.error.v1", "{body}");
    assert_eq!(json["code"], code, "{body}");
    assert!(json["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(json["hint"].as_str().is_some_and(|h| !h.is_empty()));
}

#[tokio::test]
async fn two_parts_share_one_fallback() {
    let (response, body) = send(get("/words")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body, "hello");
    let (response, body) = send(get("/numbers")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body, "7");

    for method in [Method::GET, Method::POST, Method::DELETE] {
        let request = Request::builder()
            .method(method)
            .uri("/nowhere")
            .body(Body::empty())
            .unwrap();
        let (response, body) = send(request).await;
        assert_error(&response, &body, StatusCode::NOT_FOUND, "not_found");
    }

    for path in ["/words", "/numbers"] {
        let request = Request::delete(path).body(Body::empty()).unwrap();
        let (response, body) = send(request).await;
        assert_error(
            &response,
            &body,
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
        );
        assert!(response.headers().contains_key("allow"), "{path}");
    }
}

#[tokio::test]
async fn middleware_applied_once() {
    let (response, body) = send(get("/words/id")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let ids: Vec<_> = response.headers().get_all(X_REQUEST_ID).iter().collect();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], body.as_str(), "the handler saw another request id");

    let (response, body) = send(get("/numbers/panic")).await;
    assert_error(
        &response,
        &body,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
    );
    assert!(!body.contains("panic-payload-marker"), "{body}");
}

#[test]
fn endpoints_list_every_part() {
    let endpoint = |method, path| Endpoint { method, path };
    assert_eq!(
        openxet_http::endpoints(&parts()),
        [
            endpoint(Method::GET, "/words"),
            endpoint(Method::GET, "/words/id"),
            endpoint(Method::GET, "/numbers"),
            endpoint(Method::PUT, "/numbers"),
            endpoint(Method::GET, "/numbers/panic"),
        ]
    );
}
