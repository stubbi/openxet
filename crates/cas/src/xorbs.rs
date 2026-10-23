use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes, HttpBody as _};
use axum::extract::rejection::{BytesRejection, FailedToBufferBody};
use axum::extract::{
    DefaultBodyLimit, FromRequest as _, FromRequestParts, RawPathParams, Request, State,
};
use axum::http::header::{CONTENT_LENGTH, EXPECT};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::BodyExt as _;
use openxet_auth::Principal;
use openxet_index::ChunkTable;
use openxet_protocol::{
    MAX_XORB_BODY_BYTES, MerkleHash, ProtocolError, StoredXorb, hash_from_hex, hash_to_hex,
    is_dedup_eligible, xorb_with_footer,
};
use openxet_storage::Key;
use serde::Serialize;

use crate::error::closing;
use crate::{AppState, CasError, ReadAccess, WriteAccess};

/// The only xorb prefix: what clients send (`HF_XET_DATA_DEFAULT_PREFIX`;
/// S10 "Conventions for every route") and where the bucket keeps xorbs.
const PREFIX: &str = "default";

/// [`MAX_XORB_BODY_BYTES`] as a buffer length.
#[allow(
    clippy::cast_possible_truncation,
    reason = "64.5 MiB fits the usize of every target with a 32-bit or wider address space"
)]
const BODY_LIMIT: usize = MAX_XORB_BODY_BYTES as usize;

/// `POST` and `HEAD /v1/xorbs/{prefix}/{hash}` (S10 §3).
pub(crate) fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/xorbs/{prefix}/{hash}",
        post(upload)
            .head(head)
            .layer(DefaultBodyLimit::max(BODY_LIMIT)),
    )
}

/// The xorb named by the path. A prefix other than [`PREFIX`] is
/// `404 not_found`, as for any route not served; a hash not in the spec's
/// form is `400 invalid_hash`. Neither depends on the caller, so both are
/// answered before authentication.
struct XorbPath(MerkleHash);

impl FromRequestParts<AppState> for XorbPath {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        // Raw, still percent-encoded: no encoding of `default` or of a hex
        // hash is the canonical form, so none is accepted.
        let params = RawPathParams::from_request_parts(parts, state)
            .await
            .map_err(|_| CasError::NoRoute)?;
        let (mut prefix, mut hash) = (None, None);
        for (name, value) in &params {
            match name {
                "prefix" => prefix = Some(value),
                "hash" => hash = Some(value),
                _ => {}
            }
        }
        if prefix != Some(PREFIX) {
            return Err(CasError::NoRoute);
        }
        parse_hash(hash.unwrap_or_default()).map(Self)
    }
}

/// A path hash: exactly 64 lowercase hex characters (S10 "Conventions for
/// every route"; <https://huggingface.co/docs/xet/api#converting-hashes-to-strings>).
/// `hash_from_hex` alone also takes uppercase, which no client sends and
/// which would give one xorb two URLs.
fn parse_hash(s: &str) -> Result<MerkleHash, CasError> {
    if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(CasError::InvalidHash);
    }
    hash_from_hex(s).map_err(|_| CasError::InvalidHash)
}

/// The upload answer (<https://huggingface.co/docs/xet/api#3-upload-xorb>).
/// `hf_xet` requires the field: a body without it is retried as a decode
/// error (S10 §3).
#[derive(Serialize)]
struct Uploaded {
    /// Whether the xorb was new to the caller's scope.
    was_inserted: bool,
}

/// `POST /v1/xorbs/default/{hash}`: validates the body, stores it, then
/// registers it.
///
/// - The path and the credential are checked before the body is read, and
///   so is a declared `Content-Length` over [`MAX_XORB_BODY_BYTES`].
/// - The body is validated by `xorb_with_footer` on a blocking thread,
///   holding a `validate` permit for that step only (ruling M1-R8). Its
///   computed hash must be the path's.
/// - Bucket first, then index (design §5.3). A failed put registers
///   nothing, so the index never names bytes the bucket lacks.
/// - Idempotent: `hf_xet` resends the whole body on a `5xx` or a lost
///   answer (S10 §3), and a repeat answers `200` with `was_inserted: false`.
///   `was_inserted` is per scope, so it never reveals another scope's
///   upload (design §5.4).
async fn upload(
    State(state): State<AppState>,
    request: Request,
) -> Result<Json<Uploaded>, Response> {
    let (mut parts, body) = request.into_parts();
    let (hash, principal) = match authorize(&mut parts, &state).await {
        Ok(checked) => checked,
        Err(e) => return Err(refuse_unread(e, &parts.headers, body).await),
    };
    let result = store(
        &state,
        hash,
        &principal.scope,
        Request::from_parts(parts, body),
    )
    .await;
    if let Ok(was_inserted) = &result {
        tracing::info!(xorb = %hash_to_hex(&hash), was_inserted, "xorb upload accepted");
    } else {
        tracing::debug!(xorb = %hash_to_hex(&hash), "xorb upload refused");
    }
    result.map(|was_inserted| Json(Uploaded { was_inserted }))
}

/// The upload's path, then its credential: the checks that need no body.
async fn authorize(
    parts: &mut Parts,
    state: &AppState,
) -> Result<(MerkleHash, Principal), CasError> {
    let XorbPath(hash) = XorbPath::from_request_parts(parts, state).await?;
    let WriteAccess(principal) = WriteAccess::from_request_parts(parts, state).await?;
    Ok((hash, principal))
}

/// The answer `e` to an upload refused before its body was read.
///
/// The body is read and discarded first, never buffered. A server that
/// answers and closes with request bytes unread resets the connection, and
/// the client then sees a failed body write, which it retries, instead of
/// this final answer (S10 "Status codes and retries"). Two bodies are not
/// read, and the answer closes the connection instead:
/// - one over [`MAX_XORB_BODY_BYTES`]
/// - one announced with `Expect: 100-continue`, whose client waits for the
///   server before sending it (RFC 9110 §10.1.1). The final status goes out
///   instead of `100 Continue`, which hyper sends only once the body is
///   read, so the client never sends the body.
async fn refuse_unread(e: CasError, headers: &HeaderMap, mut body: Body) -> Response {
    let expects_continue = headers
        .get(EXPECT)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"100-continue"));
    if expects_continue || body.size_hint().lower() > MAX_XORB_BODY_BYTES {
        return closing(e);
    }
    let mut read = 0u64;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            return closing(e);
        };
        if let Some(data) = frame.data_ref() {
            read = read.saturating_add(data.len() as u64);
            if read > MAX_XORB_BODY_BYTES {
                return closing(e);
            }
        }
    }
    e.into_response()
}

async fn store(
    state: &AppState,
    hash: MerkleHash,
    scope: &str,
    request: Request,
) -> Result<bool, Response> {
    // hyper's size hint is exact when the request declares a Content-Length.
    // Unlike other refusals, this body is not drained: it can't be within
    // the limit, and no stock client sends one, so the 413 goes out unread
    // and closes the connection rather than read up to 64.5 MiB for nothing.
    if request.body().size_hint().lower() > MAX_XORB_BODY_BYTES {
        return Err(closing(too_large()));
    }
    let body = Bytes::from_request(request, state)
        .await
        .map_err(|rejection| match rejection {
            BytesRejection::FailedToBufferBody(FailedToBufferBody::LengthLimitError(_)) => {
                closing(too_large())
            }
            _ => CasError::UnreadableBody.into_response(),
        })?;
    let stored = validate(state, body)
        .await
        .map_err(IntoResponse::into_response)?;
    if stored.hash != hash {
        return Err(CasError::XorbHashMismatch {
            path: hash,
            computed: stored.hash,
        }
        .into_response());
    }
    register(state, scope, stored)
        .await
        .map_err(IntoResponse::into_response)
}

fn too_large() -> CasError {
    CasError::XorbTooLarge {
        limit: MAX_XORB_BODY_BYTES,
    }
}

/// Runs `xorb_with_footer` on a blocking thread under a `validate` permit.
/// Decompression is the expensive step the permits bound (ruling M1-R8), not
/// the I/O after it, so the permit is held by the blocking task itself: it
/// is returned when validation ends, never before. A blocking task can't be
/// cancelled, so a permit held by the handler would be returned early when
/// a client disconnects mid-validation, while the work runs on unbounded.
async fn validate(state: &AppState, body: Bytes) -> Result<StoredXorb, CasError> {
    let permit = Arc::clone(&state.validate)
        .acquire_owned()
        .await
        .map_err(CasError::internal)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        xorb_with_footer(&body)
    })
    .await
    .map_err(CasError::internal)?
    .map_err(rejected)
}

/// The answer to a body `xorb_with_footer` refused: `400 xorb_<rule>` per
/// `Xorb*` variant, `413` for one over the limit.
#[allow(
    clippy::match_same_arms,
    reason = "every variant is named, so each classification is a reviewed decision"
)]
fn rejected(reason: ProtocolError) -> CasError {
    let code = match &reason {
        ProtocolError::XorbTooLarge { .. } => return too_large(),
        ProtocolError::MalformedXorb(_) => "xorb_malformed",
        ProtocolError::XorbEmpty => "xorb_empty",
        ProtocolError::XorbTooManyChunks { .. } => "xorb_too_many_chunks",
        ProtocolError::XorbChunkEmpty { .. } => "xorb_chunk_empty",
        ProtocolError::XorbChunkTooLong { .. } => "xorb_chunk_too_long",
        ProtocolError::XorbChunkTooShort { .. } => "xorb_chunk_too_short",
        ProtocolError::XorbTruncated { .. } => "xorb_truncated",
        ProtocolError::XorbTrailingBytes { .. } => "xorb_trailing_bytes",
        // Describes a stored chunk table, never an upload body.
        ProtocolError::XorbTableTooLarge { .. } => return CasError::internal(reason),
        // Not xorb rules: `xorb_with_footer` returning one is a bug. Listed
        // rather than matched by a wildcard, so a new `Xorb*` variant is a
        // compile error here instead of a 500 that clients retry.
        ProtocolError::InvalidHex(_)
        | ProtocolError::MalformedShard(_)
        | ProtocolError::ShardNotVerified(_)
        | ProtocolError::ShardFileSha256Conflict { .. }
        | ProtocolError::ShardSerialization(_)
        | ProtocolError::DedupKeyZero => return CasError::internal(reason),
    };
    CasError::InvalidXorb {
        code,
        reason: Box::new(reason),
    }
}

/// Stores the validated xorb, then registers it to `scope`. Returns whether
/// it was new to `scope`.
async fn register(state: &AppState, scope: &str, stored: StoredXorb) -> Result<bool, CasError> {
    let StoredXorb {
        hash,
        chunks,
        bytes,
        chunk_offsets,
    } = stored;
    let size = bytes.len() as u64;
    let eligible: Vec<MerkleHash> = chunks
        .hashes
        .iter()
        .map(|(chunk, _)| *chunk)
        .filter(is_dedup_eligible)
        .collect();
    let table = ChunkTable {
        hashes: chunks.hashes.into(),
        offsets: chunk_offsets.into(),
    };
    state
        .bucket
        .put(&Key::Xorb(hash), Bytes::from(bytes))
        .await?;
    // The bytes are durable from here. If the index write fails, the client
    // retries the whole upload on the 5xx: the put repeats as a no-op and
    // the registration completes. A bucket object no index row names costs
    // only space until garbage collection; the reverse never happens.
    Ok(state
        .index
        .put_xorb(&hash, size, &table, &eligible, scope)
        .await?)
}

/// `HEAD /v1/xorbs/default/{hash}`: `200` with the stored size as
/// `Content-Length` and an empty body, or `404` if the xorb isn't visible to
/// the caller's scope (<https://huggingface.co/docs/xet/api#7-head-xorb>).
async fn head(
    XorbPath(hash): XorbPath,
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
) -> Result<Response, CasError> {
    let size = state
        .index
        .xorb_size(&hash, &principal.scope, state.cfg.visibility)
        .await?
        .ok_or(CasError::XorbNotFound { xorb: hash })?;
    Ok((StatusCode::OK, [(CONTENT_LENGTH, size)]).into_response())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::{Body, to_bytes};
    use axum::http::header::{AUTHORIZATION, CONNECTION};
    use http_body_util::{BodyExt as _, Full};
    use openxet_auth::{Mode, StaticToken};
    use tower::ServiceExt as _;

    use super::*;
    use crate::{router, test_support};

    // The scheme is a constant so the repository's secret scanner, which
    // flags any literal bearer value, can tell this test token from leaks.
    const BEARER: &str = "Bearer";
    const WRITE: &str = "oxt_unit_write";

    #[tokio::test]
    async fn body_over_the_limit_without_a_length_is_413() {
        // A chunked upload declares no length, so the body limit stops the
        // read itself once it passes the limit.
        let dir = tempfile::tempdir().unwrap();
        let token = StaticToken {
            sha256: StaticToken::digest(WRITE),
            scope: "s".to_owned(),
            mode: Mode::Write,
            subject: "unit".to_owned(),
        };
        let app = router(test_support::state(dir.path(), vec![token]).await);
        let body = Full::new(Bytes::from(vec![0u8; BODY_LIMIT + 1])).map_frame(|frame| frame);
        let body = Body::new(body);
        assert_eq!(body.size_hint().upper(), None, "the length must be unknown");

        let response = app
            .oneshot(
                Request::post(format!("/v1/xorbs/default/{}", "0".repeat(64)))
                    .header(AUTHORIZATION, format!("{BEARER} {WRITE}"))
                    .body(body)
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(response.headers()[CONNECTION], "close");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "xorb_too_large");
    }

    #[tokio::test]
    async fn the_validation_permit_lives_as_long_as_the_blocking_work() {
        // A handler dropped mid-validation (the client disconnected) can't
        // cancel the blocking task, so the permit must stay with the task
        // or M1-R8's bound doesn't hold. A near-limit body of uncompressed
        // 64 KiB chunks takes tens of milliseconds to hash.
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![]).await;
        state.validate = Arc::new(tokio::sync::Semaphore::new(1));
        let chunk = {
            let mut chunk = vec![0, 0, 0, 1, 0, 0, 0, 1];
            chunk.extend(std::iter::repeat_n(7u8, 64 * 1024));
            chunk
        };
        let body = Bytes::from(chunk.repeat(1000));

        let validating = {
            let state = state.clone();
            tokio::spawn(async move { validate(&state, body).await.map(|_| ()) })
        };
        while state.validate.available_permits() == 1 {
            tokio::task::yield_now().await;
        }
        validating.abort();
        assert!(validating.await.unwrap_err().is_cancelled());

        assert_eq!(
            state.validate.available_permits(),
            0,
            "the permit was released while validation still runs"
        );
        let released = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            while state.validate.available_permits() == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        });
        released
            .await
            .expect("the permit is returned once validation ends");
    }
}
