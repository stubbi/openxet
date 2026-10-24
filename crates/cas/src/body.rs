//! Upload bodies: buffering one within a route's limit, and refusing one
//! before it is read.

use axum::body::{Body, Bytes, HttpBody as _};
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::http::header::EXPECT;
use axum::response::{IntoResponse as _, Response};
use http_body_util::BodyExt as _;

use crate::budget::Reservation;
use crate::deadline::{BodyTimedOut, timed_out};
use crate::error::closing;
use crate::{CasError, MemoryBudget};

/// The body of `request`, of at most `limit` bytes, and the memory it
/// holds, reserved from `budget` before a byte of it is read (ruling
/// M1-R40). `too_large` is the route's `413`.
///
/// - hyper's size hint is exact when the request declares a
///   Content-Length. A body declared over the limit is refused before it is
///   read: it can't be within the limit, and no stock client sends one, so
///   the `413` goes out unread and closes the connection rather than read
///   up to the limit for nothing.
/// - The reservation is the declared length, or the limit for a body that
///   declares none, and the buffer is allocated to it once, so buffering
///   never holds a second copy while it grows.
/// - A full budget refuses the body unread with `503 memory_busy`, closing
///   the connection; stock clients retry it (S10 "Status codes and
///   retries").
pub(crate) async fn buffer(
    request: Request,
    limit: u64,
    too_large: fn() -> CasError,
    budget: &MemoryBudget,
) -> Result<(Bytes, Reservation), Response> {
    let hint = request.body().size_hint();
    if hint.lower() > limit {
        return Err(closing(too_large()));
    }
    let declared = hint.upper().unwrap_or(limit).min(limit);
    let reservation = budget.reserve_upload(declared).await.map_err(closing)?;
    let mut body = request.into_body();
    let capacity = usize::try_from(declared).map_err(|e| CasError::internal(e).into_response())?;
    let mut buffer = Vec::with_capacity(capacity);
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| unreadable(&err))?;
        if let Some(data) = frame.data_ref() {
            if (buffer.len() + data.len()) as u64 > limit {
                return Err(closing(too_large()));
            }
            buffer.extend_from_slice(data);
        }
    }
    Ok((Bytes::from(buffer), reservation))
}

/// The answer to a body that failed while being read: `408` if it fell
/// behind its [`crate::BodyDeadline`], closing the connection since the
/// rest of the body is unread, and `400` otherwise.
pub(crate) fn unreadable(err: &(dyn std::error::Error + 'static)) -> Response {
    match timed_out(err) {
        Some(BodyTimedOut(deadline)) => closing(CasError::BodyTimeout(deadline)),
        None => CasError::UnreadableBody.into_response(),
    }
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
