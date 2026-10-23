//! Upload bodies: buffering one within a route's limit, and refusing one
//! before it is read.

use axum::body::{Body, Bytes, HttpBody as _};
use axum::extract::rejection::{BytesRejection, FailedToBufferBody};
use axum::extract::{FromRequest as _, Request};
use axum::http::HeaderMap;
use axum::http::header::EXPECT;
use axum::response::{IntoResponse as _, Response};
use http_body_util::BodyExt as _;

use crate::CasError;
use crate::error::closing;

/// The body of `request`, which the route's `DefaultBodyLimit` caps at
/// `limit` bytes. `too_large` is the route's `413`.
///
/// hyper's size hint is exact when the request declares a Content-Length.
/// Unlike other refusals, a body declared over the limit is not drained: it
/// can't be within the limit, and no stock client sends one, so the `413`
/// goes out unread and closes the connection rather than read up to the
/// limit for nothing.
pub(crate) async fn buffer(
    request: Request,
    limit: u64,
    too_large: fn() -> CasError,
) -> Result<Bytes, Response> {
    if request.body().size_hint().lower() > limit {
        return Err(closing(too_large()));
    }
    Bytes::from_request(request, &())
        .await
        .map_err(|rejection| match rejection {
            BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_)) => {
                closing(too_large())
            }
            _ => CasError::UnreadableBody.into_response(),
        })
}

/// The answer `e` to an upload refused before its body was read, for a
/// route whose bodies are at most `limit` bytes.
///
/// The body is read and discarded first, never buffered (ruling M1-R22). A
/// server that answers and closes with request bytes unread resets the
/// connection, and the client then sees a failed body write, which it
/// retries, instead of this final answer (S10 "Status codes and retries").
/// Two bodies are not read, and the answer closes the connection instead:
/// - one over `limit`
/// - one announced with `Expect: 100-continue`, whose client waits for the
///   server before sending it (RFC 9110 §10.1.1). The final status goes out
///   instead of `100 Continue`, which hyper sends only once the body is
///   read, so the client never sends the body.
pub(crate) async fn refuse_unread(
    e: CasError,
    headers: &HeaderMap,
    mut body: Body,
    limit: u64,
) -> Response {
    let expects_continue = headers
        .get(EXPECT)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"100-continue"));
    if expects_continue || body.size_hint().lower() > limit {
        return closing(e);
    }
    let mut read = 0u64;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return closing(e);
        };
        if let Some(data) = frame.data_ref() {
            read = read.saturating_add(data.len() as u64);
            if read > limit {
                return closing(e);
            }
        }
    }
    e.into_response()
}
