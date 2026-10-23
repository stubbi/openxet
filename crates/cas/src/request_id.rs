use std::fmt;

use axum::extract::{MatchedPath, Request};
use axum::http::HeaderValue;
use axum::middleware::Next;
use axum::response::Response;
use uuid::Uuid;

/// The response header naming the request's id. `hf_xet` reads it from
/// every response and logs it with each failure (S10 "Conventions for every
/// route").
pub const X_REQUEST_ID: &str = "x-request-id";

/// A request's id: a version 7 UUID, so ids sort by arrival time. The server
/// always assigns its own; a client-supplied `X-Request-Id` is ignored, so a
/// caller can neither forge log correlation nor inject text into logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestId(Uuid);

impl RequestId {
    fn new() -> Self {
        Self(Uuid::now_v7())
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.hyphenated().fmt(f)
    }
}

/// Assigns the request's id: in the request's extensions for the span and
/// handlers, and in the response's `X-Request-Id`, replacing any a handler
/// set.
pub(crate) async fn assign(mut request: Request, next: Next) -> Response {
    let id = RequestId::new();
    request.extensions_mut().insert(id);
    let mut response = next.run(request).await;
    let value = HeaderValue::try_from(id.to_string())
        .unwrap_or_else(|_| unreachable!("a hyphenated UUID is a valid header value"));
    response.headers_mut().insert(X_REQUEST_ID, value);
    response
}

/// The request's tracing span: the request id, the method and the matched
/// route template (`MatchedPath`, e.g. `/v1/xorbs/{prefix}/{hash}`), or
/// `unmatched` when no route matched.
///
/// Invariant: nothing from the URI, the headers or the body is ever
/// recorded. The raw path is never logged, so a credential or signature
/// in any path segment or the query is structurally unloggable; a request is
/// found by its id, which the client logs too.
pub(crate) fn span(request: &Request) -> tracing::Span {
    let id = request.extensions().get::<RequestId>();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("unmatched", MatchedPath::as_str);
    tracing::info_span!(
        "request",
        request_id = id.map(tracing::field::display),
        method = %request.method(),
        route = %route,
    )
}
