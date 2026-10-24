//! `GET /v1/chunks/{prefix}/{hash}`, global dedup (S10 §2; spec
//! <https://huggingface.co/docs/xet/api#2-query-chunk-deduplication-global-deduplication>).
//!
//! 1. the index names the xorb holding the raw chunk hash, among the xorbs
//!    visible to the caller, or the answer is `404`
//! 2. the index names the xorbs of the newest visible shard that references
//!    that xorb, in the shard's term order; the answer offers them starting
//!    at the matched xorb and wrapping round, or the matched xorb alone if
//!    no shard references it yet (ruling M1-R30, S11)
//! 3. their chunk tables are loaded, only as many as fit
//!    [`DEDUP_ANSWER_BUDGET`]
//! 4. `dedup_shard` builds a footered shard of them, its chunk hashes keyed
//!    with a fresh random key that expires in [`KEY_LIFETIME`], within that
//!    budget
//! 5. the answer is `200 application/octet-stream`
//!
//! Keying hides the raw chunk hashes, so an answer tells the caller nothing
//! about chunks it doesn't hold (design §5.9 req 4). The key is new per
//! response (ruling M1-R4), so answers can't be correlated with each other:
//! the same chunk has a different keyed hash in each. Every answer of the
//! route, a `405` included, is `Cache-Control: no-store`: each depends on
//! the caller's authorization, and a `200` is keyed per request (§5.9
//! req 3).
//!
//! Each answer holds one `dedup` permit (ruling M1-R32) from loading its
//! tables until its body has been sent, dropped or overrun its send
//! deadline (ruling M1-R33); the build runs on a blocking thread (ruling
//! M1-R25). The `dedup` permits are their own pool, a quarter of the CPUs,
//! so dedup queries can never take a permit xorb or shard validation needs.
//! A query waits at most [`PERMIT_WAIT`] for one, then answers `429
//! dedup_busy`, which the client reads as "no match"; so a client that
//! stalls its answer delays other uploads' dedup by at most that wait,
//! never the upload itself, and loses its permit at the send deadline.
//!
//! Memory: a full answer (a 16 MiB body) peaks while upstream serializes
//! it (the in-memory shard with its chunk index, and the serialization),
//! then holds the serialization and the keyed output, then the body alone
//! until it is sent. Per process, dedup memory is therefore at most the
//! `dedup` permits × about 150 MB, however many clients ask or never read.
//! Measured as true peak RSS (release, Apple M3, 8 CPUs, so 2 permits;
//! answers of 16.5 MB): about 120 MB over idle for one answer and about
//! 250 to 300 MB with both permits busy, with 8 or 16 concurrent queries
//! or 32 or 64 connections that never read (S11, "Answer cost").

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{FromRequestParts, State};
use axum::http::Method;
use axum::http::header::CONTENT_TYPE;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use openxet_index::Visibility;
use openxet_protocol::{
    DEDUP_ANSWER_BUDGET, MerkleHash, dedup_answer_xorb_bytes, dedup_shard, hash_to_hex,
};

use crate::blocking::blocking;
use crate::dedup_body::answer_body;
use crate::routes::{Routes, on};
use crate::{AppState, CasError, ReadAccess, path};

/// The prefixes served: `default`, which `hf_xet` sends, and
/// `default-merkledb`, which the spec names (S10 §2).
const PREFIXES: &[&str] = &["default", "default-merkledb"];

/// How long a response's key is valid, in seconds: 7 days (ruling M1-R4).
/// The client skips an expired shard in its cache on later sessions (S10
/// §2), so this bounds how long a cached answer is reused.
const KEY_LIFETIME: u64 = 7 * 24 * 60 * 60;

/// Xorbs whose chunk tables one index read loads. Small enough that a read
/// past the budget loads little to discard (at most about 10 MB of tables),
/// large enough that a 10 GB file's shard (about 160 xorbs) takes five
/// reads.
const TABLES_PER_LOAD: usize = 32;

/// How long a query waits for a `dedup` permit before answering `429
/// dedup_busy` (ruling M1-R33). Short, because the client waits for its
/// block's queries before it goes on: a query that waited for a permit
/// held by a stalled client would stall the upload for up to `hf_xet`'s
/// 300 s read timeout, while a `429` costs only this dedup match (S10 §2).
/// One second covers a full answer's build (about 80 ms, S11) many times.
const PERMIT_WAIT: Duration = Duration::from_secs(1);

/// `GET /v1/chunks/{prefix}/{hash}` (S10 §2).
pub(crate) fn routes() -> Routes {
    Routes::new().route(
        "/v1/chunks/{prefix}/{hash}",
        on(Method::GET, query).private(),
    )
}

/// The raw chunk hash named by the path ([`path::prefixed_hash`]).
struct ChunkPath(MerkleHash);

impl FromRequestParts<AppState> for ChunkPath {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        path::prefixed_hash(parts, state, PREFIXES).await.map(Self)
    }
}

/// Answers with the shard offering the xorbs the index names for `chunk`,
/// or `404 not_found`. A missing chunk, an ineligible one (the index holds
/// only eligible and first chunks) and, under `PerScope`, another scope's
/// all answer alike.
async fn query(
    ChunkPath(chunk): ChunkPath,
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
) -> Result<Response, CasError> {
    let (scope, vis) = (principal.scope.as_str(), state.cfg.visibility);
    let xorb = state
        .index
        .dedup_xorb(&chunk, scope, vis)
        .await?
        .ok_or(CasError::NoDedupMatch)?;
    let order = offer_order(
        state.index.dedup_shard_xorbs(&xorb, scope, vis).await?,
        xorb,
    );
    let key = fresh_key()?;
    let expiry = state.clock.now().saturating_add(KEY_LIFETIME);
    // From the table load until the body is gone, the answer holds a
    // `dedup` permit (ruling M1-R32), so its memory is bounded per process
    // and dedup can never take a permit xorb or shard validation needs.
    let permit = tokio::time::timeout(PERMIT_WAIT, Arc::clone(&state.dedup).acquire_owned())
        .await
        .map_err(|_| CasError::DedupBusy)?
        .map_err(CasError::internal)?;
    let tables = load_tables(&state, &order, scope, vis).await?;
    if tables.first().is_none_or(|(first, _)| *first != xorb) {
        // Nothing deletes a xorb in M1, so the dedup row and the chunk table
        // disagree. The client loses only this dedup match.
        tracing::warn!(
            xorb = %hash_to_hex(&xorb),
            "integrity anomaly: the index offers a dedup xorb whose chunk table it lacks"
        );
        return Err(CasError::NoDedupMatch);
    }
    // `dedup_shard` drops each table once copied and the in-memory shard
    // once serialized, so the keyed export runs with only the serialization
    // and its output alive. Every failure is a server bug: stored tables
    // within the format's counts and a non-zero key always serialize.
    let (permit, built) = blocking(permit, move || {
        let unkeyed = dedup_shard(tables, DEDUP_ANSWER_BUDGET)?;
        let offered = unkeyed.covered();
        Ok::<_, openxet_protocol::ProtocolError>((unkeyed.keyed(key, expiry)?, offered))
    })
    .await?;
    let (body, offered) = built.map_err(CasError::internal)?;
    tracing::debug!(
        xorb = %hash_to_hex(&xorb),
        offered,
        bytes = body.len(),
        "dedup match offered"
    );
    let deadline = state.cfg.send_deadline.for_len(body.len() as u64);
    let body = answer_body(body, permit, deadline);
    Ok(([(CONTENT_TYPE, "application/octet-stream")], body).into_response())
}

/// The xorbs to offer for a match in `matched`, in priority order: the
/// shard's xorbs from the matched one onwards, then the ones before it
/// (ruling M1-R30). Without a shard listing it, the matched xorb alone.
fn offer_order(shard: Option<Vec<MerkleHash>>, matched: MerkleHash) -> Vec<MerkleHash> {
    match shard {
        Some(mut xorbs) => match xorbs.iter().position(|x| *x == matched) {
            Some(at) => {
                xorbs.rotate_left(at);
                xorbs
            }
            None => vec![matched],
        },
        None => vec![matched],
    }
}

/// The chunk tables of `order`'s xorbs that are stored and visible, in
/// order, loading [`TABLES_PER_LOAD`] at a time and stopping at the first
/// table that fills [`DEDUP_ANSWER_BUDGET`]; `dedup_shard` makes the exact
/// cut.
async fn load_tables(
    state: &AppState,
    order: &[MerkleHash],
    scope: &str,
    vis: Visibility,
) -> Result<Vec<(MerkleHash, Arc<[(MerkleHash, u64)]>)>, CasError> {
    let mut tables = Vec::with_capacity(order.len());
    let mut bytes = 0;
    for batch in order.chunks(TABLES_PER_LOAD) {
        let mut loaded = state.index.chunk_tables(batch, scope, vis).await?;
        for xorb in batch {
            if let Some(table) = loaded.remove(xorb) {
                bytes += dedup_answer_xorb_bytes(table.hashes.len());
                tables.push((*xorb, table.hashes));
                if bytes >= DEDUP_ANSWER_BUDGET {
                    return Ok(tables);
                }
            }
        }
    }
    Ok(tables)
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::Body;
    use axum::http::header::AUTHORIZATION;
    use axum::http::{Request, StatusCode};
    use openxet_auth::{Mode, StaticToken};
    use openxet_index::ChunkTable;
    use openxet_protocol::{chunk_hash, is_dedup_eligible, xorb_with_footer};
    use tokio::sync::Semaphore;
    use tower::ServiceExt as _;

    use super::*;
    use crate::SendDeadline;
    use crate::test_support::PermitProbe;
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

    const WRITE: &str = "oxt_unit_write";

    fn token(secret: &str, mode: Mode) -> StaticToken {
        StaticToken {
            sha256: StaticToken::digest(secret),
            scope: "s".to_owned(),
            mode,
            subject: "unit".to_owned(),
        }
    }

    /// State over a fresh index holding one xorb with an eligible chunk,
    /// with one dedup and one validation permit, and that chunk.
    async fn one_xorb_state(dir: &std::path::Path) -> (AppState, MerkleHash) {
        let tokens = vec![token(READ, Mode::Read), token(WRITE, Mode::Write)];
        let mut state = test_support::state(dir, tokens).await;
        state.validate = Arc::new(Semaphore::new(1));
        state.dedup = Arc::new(Semaphore::new(1));
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
        (state, chunk)
    }

    fn dedup_query(chunk: &MerkleHash) -> Request<Body> {
        Request::get(format!("/v1/chunks/default/{}", hash_to_hex(chunk)))
            .header(AUTHORIZATION, format!("{BEARER} {READ}"))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn building_the_answer_takes_a_dedup_permit() {
        // Rulings M1-R25, M1-R32: dedup work is bounded by its own permits,
        // so a flood of queries can't grow the blocking pool, and the chunk
        // tables, the answer's memory, are loaded under one.
        let dir = tempfile::tempdir().unwrap();
        let (mut state, chunk) = one_xorb_state(dir.path()).await;
        let dedup = Arc::clone(&state.dedup);
        let probe = PermitProbe::install(&mut state, &dedup);
        let held = Arc::clone(&state.dedup).acquire_owned().await.unwrap();

        let answer = tokio::spawn(router(state.clone()).oneshot(dedup_query(&chunk)));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!answer.is_finished(), "answered without a permit");

        drop(held);
        let response = answer.await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            probe.free_at_load(),
            Some(0),
            "tables loaded without a permit"
        );
        drop(response);
        assert_eq!(state.dedup.available_permits(), 1);
    }

    #[tokio::test]
    async fn dedup_never_starves_validation() {
        // Review I2: with every dedup permit taken, xorb validation still
        // runs, because dedup never holds a `validate` permit.
        let dir = tempfile::tempdir().unwrap();
        let (state, chunk) = one_xorb_state(dir.path()).await;
        let held = Arc::clone(&state.dedup).acquire_owned().await.unwrap();
        let waiting = tokio::spawn(router(state.clone()).oneshot(dedup_query(&chunk)));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiting.is_finished());

        let mut body = vec![0, 4, 0, 0, 0, 4, 0, 0];
        body.extend_from_slice(b"new!");
        let xorb = xorb_with_footer(&body).unwrap().hash;
        let upload = router(state.clone())
            .oneshot(
                Request::post(format!("/v1/xorbs/default/{}", hash_to_hex(&xorb)))
                    .header(AUTHORIZATION, format!("{BEARER} {WRITE}"))
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(upload.status(), StatusCode::OK);
        assert!(
            !waiting.is_finished(),
            "the dedup query ran without a permit"
        );
        drop(held);
        assert_eq!(waiting.await.unwrap().unwrap().status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_dedup_permit_lives_until_the_body_is_gone() {
        // Review I1: an answer's body counts against the dedup permits
        // until it has been sent or dropped, so clients that never read
        // can't pile up bodies.
        let dir = tempfile::tempdir().unwrap();
        let (state, chunk) = one_xorb_state(dir.path()).await;

        let response = router(state.clone())
            .oneshot(dedup_query(&chunk))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            state.dedup.available_permits(),
            0,
            "the body outlives its permit"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(!body.is_empty());
        drop(body);
        assert_eq!(state.dedup.available_permits(), 1);

        let unread = router(state.clone())
            .oneshot(dedup_query(&chunk))
            .await
            .unwrap();
        assert_eq!(state.dedup.available_permits(), 0);
        drop(unread);
        assert_eq!(state.dedup.available_permits(), 1);
        assert_eq!(state.validate.available_permits(), 1);
    }

    #[tokio::test]
    async fn a_full_pool_answers_429_dedup_busy_within_the_wait() {
        // Ruling M1-R33: a query never waits long for a permit; `hf_xet`
        // reads a `429` from `/v1/chunks` as "no match" without retrying
        // (S10 §2), so the upload goes on at once.
        let dir = tempfile::tempdir().unwrap();
        let (state, chunk) = one_xorb_state(dir.path()).await;
        let _held = Arc::clone(&state.dedup).acquire_owned().await.unwrap();

        let start = std::time::Instant::now();
        let response = router(state.clone())
            .oneshot(dedup_query(&chunk))
            .await
            .unwrap();
        let waited = start.elapsed();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "1");
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(waited >= PERMIT_WAIT, "{waited:?}");
        assert!(waited < Duration::from_millis(1500), "{waited:?}");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["schema"], "openxet.error.v1");
        assert_eq!(json["code"], "dedup_busy");
    }

    #[test]
    fn the_send_deadline_allows_the_base_plus_the_minimum_rate() {
        // 30 s, plus 16 s for a full 16 MiB answer at 1 MiB/s.
        assert_eq!(
            SendDeadline::DEFAULT.for_len(16 << 20),
            Duration::from_secs(46)
        );
        assert_eq!(SendDeadline::DEFAULT.for_len(0), Duration::from_secs(30));
        let quick = SendDeadline::new(Duration::from_millis(100), 1000).unwrap();
        assert_eq!(quick.for_len(500), Duration::from_millis(600));
        assert!(SendDeadline::new(Duration::ZERO, 1).is_none());
        assert!(SendDeadline::new(Duration::from_secs(1), 0).is_none());
    }

    #[test]
    fn offers_start_at_the_matched_xorb_and_wrap_round() {
        let x: Vec<MerkleHash> = (0..4).map(MerkleHash::random_from_seed).collect();
        assert_eq!(
            offer_order(Some(x.clone()), x[2]),
            vec![x[2], x[3], x[0], x[1]]
        );
        assert_eq!(offer_order(Some(x.clone()), x[0]), x);
        assert_eq!(offer_order(None, x[1]), vec![x[1]]);
        // A shard list without the matched xorb (none visible to the caller
        // contains it) offers the matched xorb alone.
        assert_eq!(offer_order(Some(x[..2].to_vec()), x[3]), vec![x[3]]);
    }
}
