//! The liveness and readiness probes, and the server's answer while it
//! starts.
//!
//! Probes need no credential: orchestrators call them unauthenticated. They
//! are the one exception to design §5.9 req 1, and are safe because each
//! answers a fixed body that names nothing stored.

use axum::Router;
use axum::http::Method;
use axum::http::header::CONTENT_TYPE;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::trace::TraceLayer;

use crate::error::closing;
use crate::routes::{Routes, on};
use crate::{CasError, request_id};

const JSON: [(axum::http::HeaderName, &str); 1] = [(CONTENT_TYPE, "application/json")];

/// The probes of a server that is ready.
pub(crate) fn routes() -> Routes {
    Routes::new()
        .route("/healthz", on(Method::GET, live))
        .route("/readyz", on(Method::GET, ready))
}

/// `GET /healthz`: the process is up and serving HTTP.
async fn live() -> impl IntoResponse {
    (JSON, r#"{"status":"ok"}"#)
}

/// `GET /readyz` once the index and the bucket are open.
async fn ready() -> impl IntoResponse {
    (JSON, r#"{"status":"ready"}"#)
}

/// `503 not_ready`, closing the connection: a request body is left unread.
async fn not_ready() -> Response {
    closing(CasError::NotReady)
}

/// What a server answers while its index and bucket open, before it serves
/// [`crate::router`]: `/healthz` is `200`, and `/readyz` and every other
/// path `503 not_ready` with `Retry-After: 1`, which clients retry.
#[expect(
    clippy::disallowed_methods,
    reason = "the probes before the CAS routes exist; `router` serves the listed ones"
)]
pub fn starting_router() -> Router {
    Router::new()
        .route("/healthz", get(live))
        .route("/readyz", get(not_ready))
        .fallback(not_ready)
        .layer(TraceLayer::new_for_http().make_span_with(request_id::span))
        .layer(axum::middleware::from_fn(request_id::assign))
}
