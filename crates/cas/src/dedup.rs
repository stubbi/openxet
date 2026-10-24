//! `GET /v1/chunks/{prefix}/{hash}`, global dedup (S10 §2; spec
//! <https://huggingface.co/docs/xet/api#2-query-chunk-deduplication-global-deduplication>).
//!
//! 1. the index names the xorb to offer for the raw chunk hash, among the
//!    xorbs visible to the caller, or the answer is `404`
//! 2. the index loads that xorb's chunk table
//! 3. `dedup_shard` builds a footered shard of it, its chunk hashes keyed
//!    with a fresh random key that expires in [`KEY_LIFETIME`]
//! 4. the answer is `200 application/octet-stream`
//!
//! Keying hides the raw chunk hashes, so an answer tells the caller nothing
//! about chunks it doesn't hold (design §5.9 req 4). The key is new per
//! response (ruling M1-R4), so answers can't be correlated with each other:
//! the same chunk has a different keyed hash in each. Every answer of the
//! route is `Cache-Control: no-store`: each depends on
//! the caller's authorization, and a `200` is keyed per request (§5.9
//! req 3).
//!
//! The shard is built on a blocking thread holding a `validate` permit
//! (ruling M1-R25), so concurrent dedup work is bounded by the same
//! CPU-sized permit set as xorb and shard validation.

use std::sync::Arc;

use axum::Router;
use axum::extract::{FromRequestParts, State};
use axum::http::HeaderValue;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use openxet_protocol::{MerkleHash, dedup_shard, hash_to_hex};

use crate::blocking::blocking;
use crate::{AppState, CasError, ReadAccess, path};

/// The prefixes served: `default`, which `hf_xet` sends, and
/// `default-merkledb`, which the spec names (S10 §2).
const PREFIXES: &[&str] = &["default", "default-merkledb"];

/// How long a response's key is valid, in seconds: 7 days (ruling M1-R4).
/// The client skips an expired shard in its cache on later sessions (S10
/// §2), so this bounds how long a cached answer is reused.
const KEY_LIFETIME: u64 = 7 * 24 * 60 * 60;

/// `GET /v1/chunks/{prefix}/{hash}` (S10 §2).
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/chunks/{prefix}/{hash}", get(query))
        .route_layer(axum::middleware::map_response(no_store))
}

/// The raw chunk hash named by the path ([`path::prefixed_hash`]).
struct ChunkPath(MerkleHash);

impl FromRequestParts<AppState> for ChunkPath {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        path::prefixed_hash(parts, state, PREFIXES).await.map(Self)
    }
}

/// Answers with the shard offering the xorb the index names for `chunk`,
/// or `404 not_found`. A missing chunk, an ineligible one (the index holds
/// only eligible and first chunks) and, under `PerScope`, another scope's
/// all answer alike.
async fn query(
    ChunkPath(chunk): ChunkPath,
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
) -> Result<Response, CasError> {
    let (scope, vis) = (&principal.scope, state.cfg.visibility);
    let xorb = state
        .index
        .dedup_xorb(&chunk, scope, vis)
        .await?
        .ok_or(CasError::NoDedupMatch)?;
    let Some(table) = state
        .index
        .chunk_tables(&[xorb], scope, vis)
        .await?
        .remove(&xorb)
    else {
        // Nothing deletes a xorb in M1, so the dedup row and the chunk table
        // disagree. The client loses only this dedup match.
        tracing::warn!(
            xorb = %hash_to_hex(&xorb),
            "integrity anomaly: the index offers a dedup xorb whose chunk table it lacks"
        );
        return Err(CasError::NoDedupMatch);
    };
    let key = fresh_key()?;
    let expiry = state.clock.now().saturating_add(KEY_LIFETIME);
    // A full `MAX_XORB_CHUNKS` table builds and keys in about 1 ms in a
    // release build (measured on an Apple M3). Cheap, but a flood of
    // queries would otherwise grow the blocking pool without bound, so the
    // work holds a `validate` permit (ruling M1-R25). Every failure is a
    // server bug: a stored table within the format's counts and a non-zero
    // key always serialize.
    let permit = Arc::clone(&state.validate)
        .acquire_owned()
        .await
        .map_err(CasError::internal)?;
    let (_, body) = blocking(permit, move || {
        dedup_shard(&xorb, &table.hashes, key, expiry)
    })
    .await?;
    let body = body.map_err(CasError::internal)?;
    tracing::debug!(xorb = %hash_to_hex(&xorb), "dedup match offered");
    Ok(([(CONTENT_TYPE, "application/octet-stream")], body).into_response())
}

/// A random 32-byte HMAC key from the operating system's CSPRNG. Upstream
/// reads the all-zero key as "unkeyed", so it is drawn again; at a chance of
/// 2⁻²⁵⁶ that never happens in practice.
fn fresh_key() -> Result<MerkleHash, CasError> {
    loop {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(CasError::internal)?;
        if key != [0; 32] {
            return Ok(MerkleHash::from(key));
        }
    }
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::time::Duration;

    use axum::body::Body;
    use axum::http::header::AUTHORIZATION;
    use axum::http::{Request, StatusCode};
    use openxet_auth::{Mode, StaticToken};
    use openxet_index::ChunkTable;
    use openxet_protocol::{chunk_hash, is_dedup_eligible, xorb_with_footer};
    use tokio::sync::Semaphore;
    use tower::ServiceExt as _;

    use super::*;
    use crate::{router, test_support};

    // The scheme is a constant so the repository's secret scanner, which
    // flags any literal bearer value, can tell this test token from leaks.
    const BEARER: &str = "Bearer";
    const READ: &str = "oxt_unit_read";

    /// An uncompressed chunk whose hash is dedup eligible, framed per
    /// <https://huggingface.co/docs/xet/xorb#chunk-header-structure>.
    fn eligible_chunk() -> (Vec<u8>, MerkleHash) {
        let data = (0..1_000_000u64)
            .map(|n| n.to_le_bytes().to_vec())
            .find(|data| is_dedup_eligible(&chunk_hash(data)))
            .unwrap();
        let mut framed = vec![0, 8, 0, 0, 0, 8, 0, 0];
        framed.extend_from_slice(&data);
        (framed, chunk_hash(&data))
    }

    #[tokio::test]
    async fn building_the_answer_takes_a_validation_permit() {
        // Ruling M1-R25: dedup work is bounded by the CPU-sized `validate`
        // permits, so a flood of queries can't grow the blocking pool.
        let dir = tempfile::tempdir().unwrap();
        let token = StaticToken {
            sha256: StaticToken::digest(READ),
            scope: "s".to_owned(),
            mode: Mode::Read,
            subject: "unit".to_owned(),
        };
        let mut state = test_support::state(dir.path(), vec![token]).await;
        state.validate = Arc::new(Semaphore::new(1));
        let (body, chunk) = eligible_chunk();
        let xorb = xorb_with_footer(&body).unwrap();
        let table = ChunkTable {
            hashes: xorb.chunks.hashes.into(),
            offsets: xorb.chunk_offsets.into(),
        };
        state
            .index
            .put_xorb(&xorb.hash, 1, &table, &[chunk], "s")
            .await
            .unwrap();
        let held = Arc::clone(&state.validate).acquire_owned().await.unwrap();
        let app = router(state.clone());

        let answer = tokio::spawn(
            app.oneshot(
                Request::get(format!("/v1/chunks/default/{}", hash_to_hex(&chunk)))
                    .header(AUTHORIZATION, format!("{BEARER} {READ}"))
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!answer.is_finished(), "answered without a permit");

        drop(held);
        let response = answer.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(state.validate.available_permits(), 1);
    }
}
