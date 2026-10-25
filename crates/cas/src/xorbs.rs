use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::header::CONTENT_LENGTH;
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::Principal;
use openxet_index::ChunkTable;
use openxet_protocol::{
    MAX_XORB_BODY_BYTES, MerkleHash, ProtocolError, StoredXorb, hash_to_hex, is_dedup_eligible,
    xorb_with_footer,
};
use openxet_storage::{Key, XorbOwner};
use serde::Serialize;
use tracing::Instrument as _;

use crate::routes::{Routes, on};
use crate::{AppState, CasError, ReadAccess, WriteAccess, body, path};

/// The only xorb prefix: what clients send (`HF_XET_DATA_DEFAULT_PREFIX`;
/// S10 "Conventions for every route") and where the bucket keeps xorbs.
const PREFIX: &str = "default";

/// `POST` and `HEAD /v1/xorbs/{prefix}/{hash}` (S10 §3). Both answers
/// depend on the caller's tenant (`was_inserted`, and `HEAD` under
/// `PerTenant` visibility), so they are private.
pub(crate) fn routes() -> Routes {
    Routes::new().route(
        "/v1/xorbs/{prefix}/{hash}",
        on(Method::POST, upload).on(Method::HEAD, head).private(),
    )
}

/// The xorb named by the path, under [`PREFIX`] only
/// ([`path::prefixed_hash`]).
struct XorbPath(MerkleHash);

impl FromRequestParts<AppState> for XorbPath {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        path::prefixed_hash(parts, state, &[PREFIX]).await.map(Self)
    }
}

/// The upload answer (<https://huggingface.co/docs/xet/api#3-upload-xorb>).
/// `hf_xet` requires the field: a body without it is retried as a decode
/// error (S10 §3).
#[derive(Serialize)]
struct Uploaded {
    /// Whether the xorb was new to the caller's tenant.
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
/// - Bucket first, then index (design §5.3), under a `commit` permit
///   (ruling M1-R40). A failed put registers nothing, so the index never
///   names bytes the bucket lacks.
/// - The body's length is reserved from the memory budget before it is
///   read, until the xorb is registered ([`body::buffer`]).
/// - Idempotent: `hf_xet` resends the whole body on a `5xx` or a lost
///   answer (S10 §3), and a repeat answers `200` with `was_inserted: false`.
///   `was_inserted` is per tenant, so it never reveals another tenant's
///   upload (design §5.4).
async fn upload(
    State(state): State<AppState>,
    request: Request,
) -> Result<Json<Uploaded>, Response> {
    let (mut parts, body) = request.into_parts();
    let (hash, principal) = match authorize(&mut parts, &state).await {
        Ok(checked) => checked,
        Err(e) => {
            return Err(body::refuse_unread(e, &parts.headers, body, MAX_XORB_BODY_BYTES).await);
        }
    };
    let result = store(
        &state,
        hash,
        &principal.tenant,
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

async fn store(
    state: &AppState,
    hash: MerkleHash,
    tenant: &str,
    request: Request,
) -> Result<bool, Response> {
    let (body, reserved) = body::buffer(
        request,
        MAX_XORB_BODY_BYTES,
        too_large,
        &state.budget,
        body::Class::Upload,
    )
    .await?;
    // The work runs in its own task, which owns the body's budget share and
    // takes the commit permit, so both are held until it ends (review I-1
    // of ruling M1-R40). A client that disconnects drops this handler, but
    // can't cancel the validation or the put on their blocking threads,
    // which hold the body until they end; the upload then completes, and
    // the client's retry finds it stored.
    let work = tokio::spawn(
        {
            let (state, tenant) = (state.clone(), tenant.to_owned());
            async move {
                let _reserved = reserved;
                let stored = validate(&state, body).await?;
                if stored.hash != hash {
                    return Err(CasError::XorbHashMismatch {
                        path: hash,
                        computed: stored.hash,
                    });
                }
                register(&state, &tenant, stored).await
            }
        }
        .instrument(tracing::Span::current()),
    );
    work.await
        .map_err(|e| CasError::internal(e).into_response())?
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

/// Stores the validated xorb and `tenant`'s ownership record, then registers
/// both in the index. Returns whether `tenant` didn't own the xorb yet.
async fn register(state: &AppState, tenant: &str, stored: StoredXorb) -> Result<bool, CasError> {
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
    // Held from the bucket put until registration ends, as for shards
    // (rulings M1-R24, M1-R40): on local disk each put is a blocking task,
    // so slow storage would otherwise pile puts up on the blocking threads
    // that transfers and validation also need. The `validate` permit is
    // already returned, so slow storage never holds a CPU permit.
    let _commit = Arc::clone(&state.commit)
        .acquire_owned()
        .await
        .map_err(CasError::internal)?;
    state
        .bucket
        .put(&Key::Xorb(hash), Bytes::from(bytes))
        .await?;
    // The tenant's ownership goes to the bucket before the index records it
    // (ruling M1-R37), so a rebuilt index still knows which tenant may see
    // the xorb. The record's bytes depend only on its key, so a repeat is a
    // no-op.
    let owner = XorbOwner {
        tenant: tenant.to_owned(),
        xorb: hash,
    };
    state.bucket.put(&owner.key(), owner.to_bytes()).await?;
    // Both are durable from here. If the index write fails, the client
    // retries the whole upload on the 5xx: the puts repeat as no-ops and
    // the registration completes. A bucket object no index row names costs
    // only space until garbage collection; the reverse never happens.
    Ok(state
        .index
        .put_xorb(&hash, size, &table, &eligible, tenant)
        .await?)
}

/// `HEAD /v1/xorbs/default/{hash}`: `200` with the stored size as
/// `Content-Length` and an empty body, or `404` if the xorb isn't visible to
/// the caller's tenant (<https://huggingface.co/docs/xet/api#7-head-xorb>).
async fn head(
    XorbPath(hash): XorbPath,
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
) -> Result<Response, CasError> {
    let size = state
        .index
        .xorb_size(&hash, &principal.tenant, state.cfg.visibility)
        .await?
        .ok_or(CasError::XorbNotFound { xorb: hash })?;
    Ok((StatusCode::OK, [(CONTENT_LENGTH, size)]).into_response())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::{Body, HttpBody as _, to_bytes};
    use axum::http::header::{AUTHORIZATION, CONNECTION};
    use http_body_util::{BodyExt as _, Full};
    use openxet_auth::{CasGrant, Mode, StaticToken};
    use tower::ServiceExt as _;

    use super::*;
    use crate::test_support::{self, router};

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
            subject: "unit".to_owned(),
            cas: Some(CasGrant {
                scope: "s".to_owned(),
                mode: Mode::Write,
                tenant: None,
            }),
            user: None,
        };
        let app = router(test_support::state(dir.path(), vec![token]).await);
        let body = Full::new(Bytes::from(vec![
            0u8;
            usize::try_from(MAX_XORB_BODY_BYTES)
                .unwrap()
                + 1
        ]))
        .map_frame(|frame| frame);
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

    fn write_token() -> StaticToken {
        StaticToken {
            sha256: StaticToken::digest(WRITE),
            subject: "unit".to_owned(),
            cas: Some(CasGrant {
                scope: "s".to_owned(),
                mode: Mode::Write,
                tenant: None,
            }),
            user: None,
        }
    }

    fn post(body: Body) -> Request {
        Request::post(format!("/v1/xorbs/default/{}", "0".repeat(64)))
            .header(AUTHORIZATION, format!("{BEARER} {WRITE}"))
            .body(body)
            .unwrap()
    }

    async fn assert_memory_busy(response: Response) {
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(response.headers()[CONNECTION], "close");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "memory_busy", "{json}");
    }

    #[tokio::test]
    async fn uploads_wait_for_the_memory_budget_then_get_503_memory_busy() {
        // Ruling M1-R40: a body reserves its declared length before it is
        // read, until the request ends. With room for one 600 KiB body, a
        // second waits for the budget's wait, then is refused unread.
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![write_token()]).await;
        let wait = std::time::Duration::from_millis(300);
        state.budget =
            crate::MemoryBudget::new(1 << 20).with_waits(wait, std::time::Duration::from_mins(1));
        let app = router(state.clone());

        let (mut stalled, body) = test_support::declared_body(600 << 10);
        let first = tokio::spawn(app.clone().oneshot(post(body)));
        stalled
            .send_data(Bytes::from(vec![0; 100 << 10]))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while state.budget.available() == 1 << 20 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the first upload reserves its length");
        assert_eq!(state.budget.available(), (1 << 20) - (600 << 10));

        let (_unsent, body) = test_support::declared_body(600 << 10);
        let started = std::time::Instant::now();
        let second = app.clone().oneshot(post(body)).await.unwrap();
        assert!(started.elapsed() >= wait, "{:?}", started.elapsed());
        assert_memory_busy(second).await;

        drop(stalled);
        let first = first.await.unwrap().unwrap();
        assert_eq!(first.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.budget.available(), 1 << 20, "the request ended");
    }

    #[tokio::test]
    async fn an_upload_queues_for_the_memory_budget_far_past_the_answer_wait() {
        // A stock client sends up to 64 xorbs at once: past the budget, they
        // queue for it, unread, rather than fail with a 503 the client then
        // backs off from for seconds (S10 "Status codes and retries").
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![write_token()]).await;
        let short = std::time::Duration::from_millis(100);
        state.budget =
            crate::MemoryBudget::new(1 << 20).with_waits(std::time::Duration::from_mins(1), short);
        let app = router(state.clone());

        let (mut first_body, body) = test_support::declared_body(600 << 10);
        let first = tokio::spawn(app.clone().oneshot(post(body)));
        first_body
            .send_data(Bytes::from(vec![0; 100 << 10]))
            .await
            .unwrap();
        while state.budget.available() == 1 << 20 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let (mut second_body, body) = test_support::declared_body(600 << 10);
        let second = tokio::spawn(app.clone().oneshot(post(body)));
        tokio::time::sleep(5 * short).await;
        assert!(!second.is_finished(), "the queued upload was refused");

        drop(first_body);
        assert_eq!(
            first.await.unwrap().unwrap().status(),
            StatusCode::BAD_REQUEST
        );
        second_body
            .send_data(Bytes::from(vec![0; 600 << 10]))
            .await
            .unwrap();
        drop(second_body);
        let second = second.await.unwrap().unwrap();
        assert_eq!(
            second.status(),
            StatusCode::BAD_REQUEST,
            "read and validated"
        );
    }

    #[tokio::test]
    async fn a_body_longer_than_it_declared_is_refused() {
        // Review M-1 of M1-R40: the reservation is the declared length, so
        // a body may not grow past it, whatever the transport lets through.
        let dir = tempfile::tempdir().unwrap();
        let state = test_support::state(dir.path(), vec![write_token()]).await;
        let (mut tx, body) = test_support::declared_body(10);
        tx.send_data(Bytes::from(vec![0; 20])).await.unwrap();
        drop(tx);

        let response = router(state.clone()).oneshot(post(body)).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "bad_request", "{json}");
        assert_eq!(state.budget.available(), state.budget.bytes());
    }

    #[tokio::test]
    async fn an_upload_over_the_whole_budget_is_a_final_413_at_once() {
        // A chunked body reserves the route's limit, 64.5 MiB, which a
        // 1 MiB budget can never hold: no wait helps, and a retryable 503
        // would only be retried in vain, so the refusal is final.
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![write_token()]).await;
        state.budget = crate::MemoryBudget::new(1 << 20).with_waits(
            std::time::Duration::from_mins(1),
            std::time::Duration::from_mins(1),
        );
        let body = Body::new(Full::new(Bytes::from(vec![0u8; 10])).map_frame(|frame| frame));

        let started = std::time::Instant::now();
        let response = router(state).oneshot(post(body)).await.unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!response.headers().contains_key("retry-after"));
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["code"], "exceeds_memory_budget", "{json}");
    }

    #[tokio::test]
    async fn a_xorb_is_stored_and_registered_under_a_commit_permit() {
        // Ruling M1-R40, as M1-R24 for shards: the bucket put and the index
        // registration wait for a `commit` permit, so slow storage can't
        // pile up puts on the blocking threads.
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![write_token()]).await;
        state.commit = Arc::new(tokio::sync::Semaphore::new(1));
        let held = Arc::clone(&state.commit).acquire_owned().await.unwrap();
        let name = "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c";
        let body = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testdata/hf-xet-1.7.0-upload")
                .join(format!("{name}.xorb")),
        )
        .unwrap();
        let hash = openxet_protocol::xorb_with_footer(&body).unwrap().hash;

        let upload = tokio::spawn(
            router(state.clone()).oneshot(
                Request::post(format!("/v1/xorbs/default/{name}"))
                    .header(AUTHORIZATION, format!("{BEARER} {WRITE}"))
                    .body(Body::from(body))
                    .unwrap(),
            ),
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(!upload.is_finished(), "stored without a commit permit");
        assert_eq!(state.bucket.size(&Key::Xorb(hash)).await.unwrap(), None);

        drop(held);
        let response = upload.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(state.bucket.size(&Key::Xorb(hash)).await.unwrap().is_some());
        assert_eq!(state.commit.available_permits(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_that_leaves_mid_put_releases_nothing_until_the_put_ends() {
        // Review I-1 of M1-R40: the handler is dropped when its client
        // disconnects, but a put on a blocking thread can't be cancelled.
        // Its commit permit and its body's share of the budget must stay
        // taken until it ends, or puts pile up past both bounds.
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![write_token()]).await;
        state.commit = Arc::new(tokio::sync::Semaphore::new(2));
        let bucket =
            test_support::BlockingBucket::install(&mut state, std::time::Duration::from_secs(1));
        let whole = state.budget.bytes();
        let name = "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c";
        let body = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testdata/hf-xet-1.7.0-upload")
                .join(format!("{name}.xorb")),
        )
        .unwrap();

        let upload = tokio::spawn(
            router(state.clone()).oneshot(
                Request::post(format!("/v1/xorbs/default/{name}"))
                    .header(AUTHORIZATION, format!("{BEARER} {WRITE}"))
                    .body(Body::from(body))
                    .unwrap(),
            ),
        );
        test_support::eventually("the put starts", || {
            bucket.started.load(std::sync::atomic::Ordering::SeqCst) == 1
        })
        .await;
        upload.abort();
        assert!(upload.await.unwrap_err().is_cancelled());

        assert_eq!(bucket.finished.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(state.commit.available_permits(), 1, "commit released early");
        assert!(state.budget.available() < whole, "budget released early");
        // Two puts: the xorb, then its owner record (ruling M1-R37).
        test_support::eventually("the puts end and release both", || {
            bucket.finished.load(std::sync::atomic::Ordering::SeqCst) == 2
                && state.commit.available_permits() == 2
                && state.budget.available() == whole
        })
        .await;
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
