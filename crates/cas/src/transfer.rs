//! `GET /v1/transfer/xorbs/{xorb}`: the transfer role, which serves the byte
//! ranges reconstructions point at (ADR 0002 mode (b); ruling M1-R3).
//!
//! The signature is the only credential: the route never reads
//! `Authorization`, as stock clients send none to data URLs (S10 "Data
//! URLs"). A URL authorizes one xorb, one byte range and one expiry, and is
//! honoured only with exactly `Range: bytes={start}-{end}`
//! ([`openxet_auth::UrlSigner::check`]):
//! - no signed query: `400 invalid_transfer_url`
//! - no `Range`: `400 bad_range`
//! - a bad signature, another range or xorb, or an expired URL:
//!   `403 forbidden`, on which `hf_xet` asks for a fresh reconstruction and
//!   retries (S10 "Data URLs")
//! - more than one `Range`: `400 bad_range`
//! - otherwise `206 Partial Content` with exactly the stored bytes, read
//!   and sent in blocks of `STREAM_BLOCK_BYTES` (ruling M1-R27)
//! - `HEAD`: `405`, as no client sends it
//!
//! Every answer is `Cache-Control: no-store`. A signed range is handed to
//! one caller, and its URL carries the credential, so no shared cache may
//! keep it (design §5.9 req 3); a client re-fetching asks for a fresh
//! reconstruction anyway. Logs record the route template only, never the
//! query (ruling M1-R20).

use std::pin::Pin;
use std::task::{Context, Poll};

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::header::{ALLOW, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_core::Stream;
use futures_core::stream::BoxStream;
use openxet_auth::TransferQuery;
use openxet_protocol::{MerkleHash, hash_to_hex};
use openxet_storage::{Key, RangeStream, StorageError};

use crate::cache::private;
use crate::{AppState, CasError, path};

/// `GET /v1/transfer/xorbs/{hash}`.
pub(crate) fn routes() -> Router<AppState> {
    // `head` is set after `get`, which would otherwise answer it too.
    Router::new().route(
        "/v1/transfer/xorbs/{hash}",
        private(get(transfer).head(head_refused)),
    )
}

/// The xorb named by the path ([`path::hash`]), checked before the
/// signature.
struct XorbPath(MerkleHash);

impl FromRequestParts<AppState> for XorbPath {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        path::hash(parts, state).await.map(Self)
    }
}

async fn transfer(
    XorbPath(xorb): XorbPath,
    State(state): State<AppState>,
    query: Result<Query<TransferQuery>, QueryRejection>,
    headers: HeaderMap,
) -> Result<Response, CasError> {
    let Ok(Query(query)) = query else {
        return Err(CasError::InvalidTransferUrl);
    };
    let (start, end) =
        state
            .urls
            .check(&xorb, &query, range_header(&headers)?, state.clock.now())?;
    let RangeStream { size, blocks } = state
        .bucket
        .get_range_stream(&Key::Xorb(xorb), start, end)
        .await
        .map_err(|e| refused_read(&xorb, e))?;
    // A streamed body holds one block at a time, never the range (ruling
    // M1-R27). It is sent with its length, so a read failing partway cuts
    // the response short, which the client retries; it can't read as whole.
    let body = Body::from_stream(Logged {
        blocks,
        xorb: hash_to_hex(&xorb),
    });
    Ok((
        StatusCode::PARTIAL_CONTENT,
        [
            (CONTENT_TYPE, "application/octet-stream".to_owned()),
            (CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
            (CONTENT_LENGTH, (end - start + 1).to_string()),
        ],
        body,
    )
        .into_response())
}

/// The answer to a read the bucket refused before streaming. The signer
/// signs only ranges planned from the index's chunk table, so a range past
/// the stored xorb means the table and the bucket disagree: an integrity
/// fault, `500`, never the caller's.
fn refused_read(xorb: &MerkleHash, e: StorageError) -> CasError {
    if let StorageError::RangeOutOfBounds {
        start,
        end_inclusive,
        size,
        ..
    } = &e
    {
        tracing::error!(
            xorb = %hash_to_hex(xorb),
            start,
            end_inclusive,
            size,
            "integrity anomaly: a signed transfer range lies past the stored xorb"
        );
        return CasError::internal(e);
    }
    e.into()
}

/// A range's blocks, logging a read that fails partway: the client sees
/// only a cut-short body.
struct Logged {
    blocks: BoxStream<'static, Result<Bytes, StorageError>>,
    xorb: String,
}

impl Stream for Logged {
    type Item = Result<Bytes, StorageError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let next = self.blocks.as_mut().poll_next(cx);
        if let Poll::Ready(Some(Err(e))) = &next {
            tracing::error!(
                xorb = %self.xorb,
                error = e as &dyn std::error::Error,
                "a transfer's read failed partway"
            );
        }
        next
    }
}

/// `HEAD` is refused: no client sends it, and answering it would read the
/// range for nothing.
async fn head_refused() -> Response {
    let mut response = CasError::MethodNotAllowed.into_response();
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static("GET"));
    response
}

/// The request's one `Range` value; one that isn't visible ASCII matches no
/// signed range, so it reads as empty, which the check refuses (`403`).
///
/// # Errors
///
/// [`CasError::BadRange`] for more than one `Range` header: no range.
fn range_header(headers: &HeaderMap) -> Result<Option<&str>, CasError> {
    let mut values = headers.get_all(RANGE).iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(CasError::BadRange);
    }
    Ok(Some(first.to_str().unwrap_or("")))
}
