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
//! - otherwise `206 Partial Content` with exactly the stored bytes
//!
//! Every answer is `Cache-Control: no-store`. A signed range is handed to
//! one caller, and its URL carries the credential, so no shared cache may
//! keep it (design §5.9 req 3); a client re-fetching asks for a fresh
//! reconstruction anyway. Logs record the route template only, never the
//! query (ruling M1-R20).

use axum::Router;
use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::header::{CONTENT_RANGE, CONTENT_TYPE, RANGE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use openxet_auth::TransferQuery;
use openxet_protocol::{MerkleHash, hash_to_hex};
use openxet_storage::{Key, StorageError};

use crate::cache::private;
use crate::{AppState, CasError, path};

/// `GET /v1/transfer/xorbs/{hash}`.
pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/v1/transfer/xorbs/{hash}", private(get(transfer)))
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
            .check(&xorb, &query, range_header(&headers), state.clock.now())?;
    let key = Key::Xorb(xorb);
    let size = state
        .bucket
        .size(&key)
        .await?
        .ok_or(StorageError::NotFound { key })?;
    if end >= size {
        // The signer signs only ranges planned from the index's chunk
        // table, so the table and the stored xorb disagree.
        tracing::error!(
            xorb = %hash_to_hex(&xorb),
            start,
            end,
            size,
            "integrity anomaly: a signed transfer range lies past the stored xorb"
        );
        return Err(CasError::RangeNotSatisfiable { size });
    }
    let bytes = state.bucket.get_range(&key, start, end).await?;
    Ok((
        StatusCode::PARTIAL_CONTENT,
        [
            (CONTENT_TYPE, "application/octet-stream".to_owned()),
            (CONTENT_RANGE, format!("bytes {start}-{end}/{size}")),
        ],
        bytes,
    )
        .into_response())
}

/// The request's one `Range` value. Several, or one that isn't visible
/// ASCII, match no signed range: they read as an empty value, which the
/// check refuses as a mismatch.
fn range_header(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(RANGE).iter();
    let first = values.next()?;
    if values.next().is_some() {
        return Some("");
    }
    Some(first.to_str().unwrap_or(""))
}
