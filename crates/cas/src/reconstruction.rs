//! File reconstruction and `HEAD /v1/files` (S10 §1, §5; spec
//! <https://huggingface.co/docs/xet/api#1-get-file-reconstruction>,
//! <https://huggingface.co/docs/xet/api#6-batch-file-reconstruction> and
//! <https://huggingface.co/docs/xet/api#8-head-file>):
//! - `GET /v2/reconstructions/{file}` and `GET /v1/reconstructions/{file}`,
//!   with an optional `Range`
//! - `POST /v1/reconstructions` with a JSON list of keys, and
//!   `GET /v1/reconstructions?file_id=…&file_id=…`, which answer alike
//! - `HEAD /v1/files/{file}`
//!
//! A file is answered only to the scope it was registered to (design §5.4,
//! ruling M1-R16); for any other caller it reads as never stored, `404`.
//! Each answer is planned by [`crate::plan`] and every fetch descriptor gets
//! its own URL to the transfer route, signed for exactly its byte range
//! (ADR 0002). A `validate` permit is held from loading the chunk layouts to
//! the serialized answer, and planning, signing and serialization run on a
//! blocking thread, as other CPU-bound request work does (rulings M1-R8,
//! M1-R25): a file of 100k terms signs 100k URLs. A batch is capped at
//! [`MAX_BATCH_FILES`] files and [`MAX_BATCH_TERMS`] terms.
//!
//! Every answer, errors included, is `Cache-Control: no-store`: it depends
//! on the caller's scope and carries URLs that act as credentials (design
//! §5.9 req 3).

use std::collections::{HashSet, VecDeque};
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, FromRequestParts, RawQuery, Request, State};
use axum::handler::Handler as _;
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, RANGE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use http_body::{Frame, SizeHint};
use openxet_index::ChunkLayout;
use openxet_protocol::{MerkleHash, hash_to_hex};
use serde::Deserialize;
use tokio::sync::OwnedSemaphorePermit;

use crate::answer::{self, Signer, Version};
use crate::blocking::blocking;
use crate::plan::{self, PlanError, XorbMap, XorbSet};
use crate::routes::{Routes, on};
use crate::{AppState, CasError, ReadAccess, body, path};

/// The most distinct files one batch reconstruction may name; a batch over
/// it is `400 batch_too_large`. Every file in a batch is planned whole, so
/// this bounds the work and the answer of one request.
pub const MAX_BATCH_FILES: usize = 1000;

/// The most terms one batch reconstruction may plan, over all the files it
/// is answered for; a batch over it is `400 batch_too_large`. With
/// [`MAX_BATCH_FILES`], it bounds one batch's chunk layouts, signing and
/// answer, whatever the files hold.
pub const MAX_BATCH_TERMS: usize = 1_000_000;

// `CasError::BatchTooLarge`'s hint names the cap.
const _: () = assert!(MAX_BATCH_FILES == 1000);

/// The largest batch body read, in bytes: [`MAX_BATCH_FILES`] keys of about
/// 100 bytes each as the spec writes them, with room for whitespace.
const BATCH_BODY_LIMIT: u64 = 256 * 1024;

/// The reconstruction routes and `HEAD /v1/files/{hash}`.
pub(crate) fn routes() -> Routes {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "256 KiB fits the usize of every target"
    )]
    let batch_limit = DefaultBodyLimit::max(BATCH_BODY_LIMIT as usize);
    Routes::new()
        .route("/v2/reconstructions/{hash}", on(Method::GET, v2).private())
        .route("/v1/reconstructions/{hash}", on(Method::GET, v1).private())
        .route(
            "/v1/reconstructions",
            on(Method::POST, batch_post.layer(batch_limit))
                .on(Method::GET, batch_get)
                .private(),
        )
        .route("/v1/files/{hash}", on(Method::HEAD, head_file).private())
}

/// The file named by the path ([`path::hash`]).
struct FilePath(MerkleHash);

impl FromRequestParts<AppState> for FilePath {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        path::hash(parts, state).await.map(Self)
    }
}

/// `GET /v2/reconstructions/{file}`, the route `hf_xet` prefers.
async fn v2(
    FilePath(file): FilePath,
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, CasError> {
    single(&state, &principal.scope, file, &headers, Version::V2).await
}

/// `GET /v1/reconstructions/{file}`: the same terms and URLs as v2, in the
/// v1 shape.
async fn v1(
    FilePath(file): FilePath,
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, CasError> {
    single(&state, &principal.scope, file, &headers, Version::V1).await
}

/// One file's reconstruction for `scope`, for the request's `Range`:
/// - `404 not_found` if the file isn't registered to `scope`
/// - `416 range_not_satisfiable` if the range starts at or past its end;
///   the empty file has no satisfiable range, and without a `Range` it is
///   an answer with no terms (S10 §1)
/// - `400 invalid_range` for a `Range` that isn't one byte range
/// - `500` if the index's record disagrees with its chunk layouts, never a
///   partial plan
async fn single(
    state: &AppState,
    scope: &str,
    file: MerkleHash,
    headers: &HeaderMap,
    version: Version,
) -> Result<Response, CasError> {
    let requested = requested_range(headers)?;
    let (record, bytes) = state
        .index
        .file_with_bytes(&file, scope)
        .await?
        .ok_or(CasError::FileNotFound)?;
    let range = resolve(requested, record.size)?;
    let window = plan::window(&record, range).map_err(planning_failed)?;
    let terms = window.as_ref().map_or(0, |w| w.terms.len());
    // With the terms' recorded bytes, only the terms trimmed to the range
    // need their xorbs' layouts.
    let xorbs = match (&window, &bytes) {
        (None, _) => Vec::new(),
        (Some(w), Some(_)) => w
            .trimmed(&record)
            .iter()
            .map(|&t| record.terms[t].xorb)
            .collect(),
        (Some(w), None) => distinct_xorbs(&record.terms[w.terms.clone()]),
    };
    let permit = validation_permit(state).await?;
    let tables = chunk_layouts(state, scope, xorbs).await?;

    let signer = Signer::new(state);
    let body = render(state, permit, terms, move |workers| {
        let plan = plan::plan(&record, bytes.as_ref(), &tables, range).map_err(planning_failed)?;
        answer::single(&plan, version, &signer, workers)
    })
    .await?;
    tracing::debug!(file = %hash_to_hex(&file), "reconstruction answered");
    Ok(json(body))
}

/// One key of a `POST /v1/reconstructions` body
/// (<https://huggingface.co/docs/xet/api#6-batch-file-reconstruction>).
#[derive(Deserialize)]
struct BatchKey {
    prefix: String,
    hash: String,
}

/// `POST /v1/reconstructions` with `[{"prefix":"default","hash":…}, …]`,
/// as the spec writes the batch. The caller is authorized before the body
/// is read; a refused body is drained (ruling M1-R22).
async fn batch_post(State(state): State<AppState>, request: Request) -> Result<Response, Response> {
    let (mut parts, body) = request.into_parts();
    let principal = match ReadAccess::from_request_parts(&mut parts, &state).await {
        Ok(ReadAccess(principal)) => principal,
        Err(e) => return Err(body::refuse_unread(e, &parts.headers, body, BATCH_BODY_LIMIT).await),
    };
    let body = body::buffer(
        Request::from_parts(parts, body),
        BATCH_BODY_LIMIT,
        batch_too_long,
    )
    .await?;
    let keys: Vec<BatchKey> =
        serde_json::from_slice(&body).map_err(|_| CasError::InvalidBatch.into_response())?;
    let mut files = Vec::with_capacity(keys.len());
    for key in keys {
        if key.prefix != "default" {
            return Err(CasError::InvalidBatch.into_response());
        }
        files.push(path::parse_hash(&key.hash).map_err(IntoResponse::into_response)?);
    }
    batch(&state, &principal.scope, files)
        .await
        .map_err(IntoResponse::into_response)
}

fn batch_too_long() -> CasError {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "256 KiB fits the usize of every target"
    )]
    CasError::PayloadTooLarge {
        limit: BATCH_BODY_LIMIT as usize,
    }
}

/// `GET /v1/reconstructions?file_id={hash}&file_id={hash}…`, as
/// `xet-client` 1.7.0 sends the batch (S10 §1). Other query parameters are
/// ignored. A URI is at most 64 KiB (the `http` crate's limit, in clients
/// and in hyper), so a GET names at most about 880 files; the POST form
/// reaches [`MAX_BATCH_FILES`].
async fn batch_get(
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Response, CasError> {
    let files = url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
        .filter(|(name, _)| name == "file_id")
        .map(|(_, value)| path::parse_hash(&value))
        .collect::<Result<Vec<_>, _>>()?;
    batch(&state, &principal.scope, files).await
}

/// The batch answer for `files`, in the v1 shapes (S10 §1):
/// `{"files": {file: [term…]}, "fetch_info": {xorb: [entry…]}}`. Each file
/// is planned whole. Files not registered to `scope` are left out, so the
/// answer is the same as for files never stored; a repeated file is
/// answered once.
async fn batch(
    state: &AppState,
    scope: &str,
    files: Vec<MerkleHash>,
) -> Result<Response, CasError> {
    let mut seen = HashSet::with_capacity(files.len());
    let files: Vec<MerkleHash> = files.into_iter().filter(|f| seen.insert(*f)).collect();
    if files.len() > MAX_BATCH_FILES {
        return Err(CasError::BatchTooLarge {
            count: files.len(),
            limit: MAX_BATCH_FILES,
        });
    }
    let mut found = state.index.files(&files, scope).await?;
    let records: Vec<_> = files.iter().filter_map(|f| found.remove(f)).collect();
    let terms: usize = records.iter().map(|r| r.terms.len()).sum();
    if terms > MAX_BATCH_TERMS {
        return Err(CasError::BatchTooManyTerms {
            terms,
            limit: MAX_BATCH_TERMS,
        });
    }
    let xorbs = distinct_xorbs(records.iter().flat_map(|r| &r.terms));
    let permit = validation_permit(state).await?;
    let tables = chunk_layouts(state, scope, xorbs).await?;

    let signer = Signer::new(state);
    let body = render(state, permit, terms, move |workers| {
        let plans = records
            .iter()
            .map(|record| {
                plan::plan(record, None, &tables, None)
                    .map(|plan| (record.hash, plan))
                    .map_err(planning_failed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        answer::batch(&plans, &signer, workers)
    })
    .await?;
    tracing::debug!(files = files.len(), "batch reconstruction answered");
    Ok(json(body))
}

/// `HEAD /v1/files/{file}`: `200` with the file's size as `Content-Length`
/// and an empty body, or `404` if it isn't registered to the caller's scope
/// (<https://huggingface.co/docs/xet/api#8-head-file>).
async fn head_file(
    FilePath(file): FilePath,
    ReadAccess(principal): ReadAccess,
    State(state): State<AppState>,
) -> Result<Response, CasError> {
    let record = state
        .index
        .file(&file, &principal.scope)
        .await?
        .ok_or(CasError::FileNotFound)?;
    Ok((StatusCode::OK, [(CONTENT_LENGTH, record.size)]).into_response())
}

/// A reconstruction `Range`, before the file's size is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ByteRange {
    /// `bytes=start-end` (`end` inclusive) or `bytes=start-`.
    From { start: u64, end: Option<u64> },
    /// `bytes=-length`: the last `length` bytes.
    Suffix(u64),
}

/// The request's `Range` (RFC 9110 §14.1.2), which `hf_xet` always sends as
/// `bytes=start-end` (S10 §1). Open-ended and suffix ranges are served as
/// upstream's reference server serves them (`xet-client` 1.7.0,
/// `local_server/handlers.rs`). More than one range, another unit, or a
/// `start` after `end` is `400 invalid_range`.
pub(crate) fn requested_range(headers: &HeaderMap) -> Result<Option<ByteRange>, CasError> {
    let mut values = headers.get_all(RANGE).iter();
    let Some(value) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(CasError::InvalidRange);
    }
    let value = value.to_str().map_err(|_| CasError::InvalidRange)?;
    let (unit, spec) = value.split_once('=').ok_or(CasError::InvalidRange)?;
    if !unit.trim().eq_ignore_ascii_case("bytes") {
        return Err(CasError::InvalidRange);
    }
    let (start, end) = spec.trim().split_once('-').ok_or(CasError::InvalidRange)?;
    let number = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse::<u64>().ok())
            .flatten()
            .ok_or(CasError::InvalidRange)
    };
    match (start.is_empty(), end.is_empty()) {
        (true, true) => Err(CasError::InvalidRange),
        (true, false) => Ok(Some(ByteRange::Suffix(number(end)?))),
        (false, true) => Ok(Some(ByteRange::From {
            start: number(start)?,
            end: None,
        })),
        (false, false) => {
            let (start, end) = (number(start)?, number(end)?);
            if end < start {
                return Err(CasError::InvalidRange);
            }
            Ok(Some(ByteRange::From {
                start,
                end: Some(end),
            }))
        }
    }
}

/// `range` as the inclusive byte range [`plan::plan`] takes for a file of
/// `size` bytes; its end may lie past the file, which planning clamps.
pub(crate) fn resolve(range: Option<ByteRange>, size: u64) -> Result<Option<(u64, u64)>, CasError> {
    match range {
        None => Ok(None),
        Some(ByteRange::From { start, end }) => Ok(Some((start, end.unwrap_or(u64::MAX)))),
        // No suffix of the empty file, nor an empty suffix, is satisfiable
        // (RFC 9110 §14.1.2).
        Some(ByteRange::Suffix(0)) => Err(CasError::RangeNotSatisfiable { size }),
        Some(ByteRange::Suffix(_)) if size == 0 => Err(CasError::RangeNotSatisfiable { size }),
        Some(ByteRange::Suffix(len)) => Ok(Some((size - len.min(size), u64::MAX))),
    }
}

/// The distinct xorbs `terms` name, in order of first use.
fn distinct_xorbs<'a>(terms: impl IntoIterator<Item = &'a openxet_index::Term>) -> Vec<MerkleHash> {
    let mut seen = XorbSet::default();
    terms
        .into_iter()
        .map(|t| t.xorb)
        .filter(|x| seen.insert(*x))
        .collect()
}

/// The chunk layouts of `xorbs`, every one of which a file registered to
/// `scope` names. Registration checked each was visible to `scope` (ruling
/// M1-R17), so a missing one is an index fault: `500`, logged as an
/// integrity anomaly.
async fn chunk_layouts(
    state: &AppState,
    scope: &str,
    xorbs: Vec<MerkleHash>,
) -> Result<XorbMap<ChunkLayout>, CasError> {
    let tables = state
        .index
        .chunk_layouts(&xorbs, scope, state.cfg.visibility)
        .await?;
    if let Some(missing) = xorbs.iter().find(|x| !tables.contains_key(x)) {
        return Err(CasError::internal(format!(
            "integrity anomaly: a registered file names xorb {}, whose chunk layout the index \
             doesn't hold",
            hash_to_hex(missing)
        )));
    }
    Ok(tables.into_iter().collect())
}

/// The answer to a [`PlanError`]. An inconsistent record is logged, with
/// its detail, as the `500`'s cause.
fn planning_failed(e: PlanError) -> CasError {
    match e {
        PlanError::Unsatisfiable { size } => CasError::RangeNotSatisfiable { size },
        PlanError::Inconsistent(detail) => {
            CasError::internal(format!("integrity anomaly: {detail}"))
        }
    }
}

/// A `validate` permit, taken before a plan's chunk layouts are loaded and
/// held until its answer is serialized: the layouts are much of a large
/// plan's memory, and planning, signing and serializing its CPU (rulings
/// M1-R8, M1-R25).
async fn validation_permit(state: &AppState) -> Result<OwnedSemaphorePermit, CasError> {
    Arc::clone(&state.validate)
        .acquire_owned()
        .await
        .map_err(CasError::internal)
}

/// Runs `work` on a blocking thread holding `permit` ([`blocking`]), for an
/// answer of `terms` terms. `work` gets how many threads may write the
/// answer: one, plus one per extra `validate` permit.
///
/// Extra permits (ruling M1-R34):
/// - are taken only if free right now, never waited for, so a reconstruction
///   never jumps uploads queued for a permit
/// - number at most half, rounded down, of the permits that were free
///   counting the request's own, and at most what the answer is worth
///   ([`answer::workers_for`]). Of `f` such permits the request holds at
///   most `1 + f / 2`, so uploads keep the other `f.div_ceil(2) - 1`: on 4
///   free permits an answer gets 3 writers, on 8 it gets 5 and leaves 3.
/// - are held until `work` returns, error or not, so the threads stay
///   within the CPU bound of rulings M1-R8 and M1-R25
///
/// A busy server, with no permit free beside the request's own, writes the
/// answer on one thread.
async fn render(
    state: &AppState,
    permit: OwnedSemaphorePermit,
    terms: usize,
    work: impl FnOnce(usize) -> Result<Vec<Bytes>, CasError> + Send + 'static,
) -> Result<Vec<Bytes>, CasError> {
    let free = state.validate.available_permits() + 1;
    let wanted = (answer::workers_for(terms) - 1).min(free / 2);
    let extra = (1..=wanted).rev().find_map(|n| {
        let n = u32::try_from(n).ok()?;
        let permits = Arc::clone(&state.validate).try_acquire_many_owned(n).ok()?;
        Some((permits, n as usize))
    });
    blocking(permit, move || {
        let (_held, workers) = extra.map_or((None, 1), |(permits, n)| (Some(permits), 1 + n));
        work(workers)
    })
    .await?
    .1
}

/// A JSON answer sent as `parts`, with their total as its Content-Length.
fn json(parts: Vec<Bytes>) -> Response {
    (
        [(CONTENT_TYPE, "application/json")],
        Body::new(Frames(parts.into())),
    )
        .into_response()
}

/// A body of the given frames, of an exact size known up front, so the
/// server sends a Content-Length as for one buffer.
struct Frames(VecDeque<Bytes>);

impl http_body::Body for Frames {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        Poll::Ready(self.0.pop_front().map(|part| Ok(Frame::data(part))))
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_empty()
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.0.iter().map(|part| part.len() as u64).sum())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::HeaderValue;
    use axum::http::header::AUTHORIZATION;
    use openxet_auth::{Mode, StaticToken};
    use openxet_index::{
        ChunkTable, FileRecord, Index, IndexError, NewFile, Term, TermBytes, Visibility,
    };
    use tokio::sync::Semaphore;
    use tower::ServiceExt as _;

    use super::*;
    use crate::{router, test_support};

    // The scheme is a constant so the repository's secret scanner, which
    // flags any literal bearer value, can tell this test token from leaks.
    const BEARER: &str = "Bearer";
    const READ: &str = "oxt_unit_read";

    /// An index counting its `chunk_tables` and `chunk_layouts` calls.
    struct CountingIndex {
        inner: Arc<dyn Index>,
        table_loads: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Index for CountingIndex {
        async fn put_xorb(
            &self,
            xorb: &MerkleHash,
            size: u64,
            table: &ChunkTable,
            eligible: &[MerkleHash],
            scope: &str,
        ) -> Result<bool, IndexError> {
            self.inner
                .put_xorb(xorb, size, table, eligible, scope)
                .await
        }

        async fn xorb_size(
            &self,
            xorb: &MerkleHash,
            scope: &str,
            vis: Visibility,
        ) -> Result<Option<u64>, IndexError> {
            self.inner.xorb_size(xorb, scope, vis).await
        }

        async fn chunk_tables(
            &self,
            xorbs: &[MerkleHash],
            scope: &str,
            vis: Visibility,
        ) -> Result<HashMap<MerkleHash, ChunkTable>, IndexError> {
            self.table_loads.fetch_add(1, Ordering::SeqCst);
            self.inner.chunk_tables(xorbs, scope, vis).await
        }

        async fn chunk_layouts(
            &self,
            xorbs: &[MerkleHash],
            scope: &str,
            vis: Visibility,
        ) -> Result<HashMap<MerkleHash, ChunkLayout>, IndexError> {
            self.table_loads.fetch_add(1, Ordering::SeqCst);
            self.inner.chunk_layouts(xorbs, scope, vis).await
        }

        async fn register_shard(
            &self,
            shard: &MerkleHash,
            files: &[NewFile],
            scope: &str,
            vis: Visibility,
        ) -> Result<bool, IndexError> {
            self.inner.register_shard(shard, files, scope, vis).await
        }

        async fn file(
            &self,
            file: &MerkleHash,
            scope: &str,
        ) -> Result<Option<FileRecord>, IndexError> {
            self.inner.file(file, scope).await
        }

        async fn files(
            &self,
            files: &[MerkleHash],
            scope: &str,
        ) -> Result<HashMap<MerkleHash, FileRecord>, IndexError> {
            self.inner.files(files, scope).await
        }

        async fn file_with_bytes(
            &self,
            file: &MerkleHash,
            scope: &str,
        ) -> Result<Option<(FileRecord, Option<TermBytes>)>, IndexError> {
            self.inner.file_with_bytes(file, scope).await
        }

        async fn dedup_xorb(
            &self,
            chunk: &MerkleHash,
            scope: &str,
            vis: Visibility,
        ) -> Result<Option<MerkleHash>, IndexError> {
            self.inner.dedup_xorb(chunk, scope, vis).await
        }

        async fn dedup_shard_xorbs(
            &self,
            xorb: &MerkleHash,
            scope: &str,
            vis: Visibility,
        ) -> Result<Option<Vec<MerkleHash>>, IndexError> {
            self.inner.dedup_shard_xorbs(xorb, scope, vis).await
        }
    }

    #[tokio::test]
    async fn chunk_tables_are_loaded_under_a_validation_permit() {
        // Tables are the bulk of a large plan's memory, so the CPU-sized
        // `validate` permits bound them too, not only planning.
        let dir = tempfile::tempdir().unwrap();
        let token = StaticToken {
            sha256: StaticToken::digest(READ),
            scope: "s".to_owned(),
            mode: Mode::Read,
            subject: "unit".to_owned(),
        };
        let mut state = test_support::state(dir.path(), vec![token]).await;
        let table_loads = Arc::new(AtomicUsize::new(0));
        state.index = Arc::new(CountingIndex {
            inner: Arc::clone(&state.index),
            table_loads: Arc::clone(&table_loads),
        });
        state.validate = Arc::new(Semaphore::new(1));
        let xorb = openxet_protocol::chunk_hash(b"xorb");
        let table = ChunkTable {
            hashes: vec![(openxet_protocol::chunk_hash(b"c"), 1)].into(),
            offsets: vec![0, 9].into(),
        };
        state
            .index
            .put_xorb(&xorb, 9, &table, &[], "s")
            .await
            .unwrap();
        let file = FileRecord {
            hash: openxet_protocol::chunk_hash(b"file"),
            size: 1,
            terms: vec![Term {
                xorb,
                chunk_start: 0,
                chunk_end: 1,
                unpacked_bytes: 1,
            }],
        };
        let new = NewFile {
            record: file.clone(),
            sha256: None,
            first_chunk: None,
        };
        state
            .index
            .register_shard(&xorb, &[new], "s", Visibility::Global)
            .await
            .unwrap();
        let held = Arc::clone(&state.validate).acquire_owned().await.unwrap();
        let single = Request::get(format!("/v2/reconstructions/{}", hash_to_hex(&file.hash)));
        let batch = Request::get(format!(
            "/v1/reconstructions?file_id={}",
            hash_to_hex(&file.hash)
        ));

        for request in [single, batch] {
            let answer = tokio::spawn(
                router(state.clone()).oneshot(
                    request
                        .header(AUTHORIZATION, format!("{BEARER} {READ}"))
                        .body(Body::empty())
                        .unwrap(),
                ),
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(!answer.is_finished(), "answered without a permit");
            assert_eq!(
                table_loads.load(Ordering::SeqCst),
                0,
                "tables loaded without a permit"
            );
            answer.abort();
        }
        drop(held);
    }

    /// The writer threads `render` grants a 100k-term answer when `held`
    /// of 8 `validate` permits are taken elsewhere, and the permits free
    /// while it writes; asserts every permit comes back after.
    async fn writers_with(held: u32, fails: bool) -> (usize, usize) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![]).await;
        state.validate = Arc::new(Semaphore::new(8));
        let others = Arc::clone(&state.validate)
            .acquire_many_owned(held)
            .await
            .unwrap();
        let permit = validation_permit(&state).await.unwrap();
        let validate = Arc::clone(&state.validate);
        let seen = Arc::new(std::sync::Mutex::new((0, 0)));
        let record = Arc::clone(&seen);
        let result = render(&state, permit, 100_000, move |workers| {
            *record.lock().unwrap() = (workers, validate.available_permits());
            if fails {
                return Err(CasError::internal("work failed"));
            }
            Ok(vec![Bytes::from_static(b"{}")])
        })
        .await;
        assert_eq!(result.is_err(), fails);
        assert_eq!(
            state.validate.available_permits(),
            8 - held as usize,
            "render kept permits"
        );
        drop(others);
        *seen.lock().unwrap()
    }

    #[tokio::test]
    async fn extra_writers_take_at_most_half_the_free_permits_and_return_them() {
        // Ruling M1-R34. With 3 of 8 held, 5 were free counting the
        // request's own: 2 extra writers, and 2 permits stay free.
        assert_eq!(writers_with(3, false).await, (3, 2));
        assert_eq!(
            writers_with(3, true).await,
            (3, 2),
            "an error returns them too"
        );
        // All 8 free: 4 extra, 3 stay free.
        assert_eq!(writers_with(0, false).await, (5, 3));
        // 4 free, as on a 4-vCPU node: 3 writers.
        assert_eq!(writers_with(4, false).await, (3, 1));
    }

    #[tokio::test]
    async fn a_busy_pool_writes_on_one_thread() {
        // Only the request's own permit was free.
        assert_eq!(writers_with(7, false).await, (1, 0));
        // One more: half of 2 is one extra.
        assert_eq!(writers_with(6, false).await, (2, 0));
    }

    /// A clock stopped at one second, so two answers sign the same URLs.
    struct Stopped;

    impl crate::Clock for Stopped {
        fn now(&self) -> u64 {
            1_800_000_000
        }
    }

    #[tokio::test]
    async fn an_answer_written_on_one_thread_is_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let token = StaticToken {
            sha256: StaticToken::digest(READ),
            scope: "s".to_owned(),
            mode: Mode::Read,
            subject: "unit".to_owned(),
        };
        let mut state = test_support::state(dir.path(), vec![token]).await;
        state.validate = Arc::new(Semaphore::new(8));
        state.clock = Arc::new(Stopped);
        let xorbs: Vec<MerkleHash> = (0..2u8)
            .map(|x| openxet_protocol::chunk_hash(&[x]))
            .collect();
        let table = ChunkTable {
            hashes: (0..1024u32)
                .map(|c| (openxet_protocol::chunk_hash(&c.to_le_bytes()), 100))
                .collect(),
            offsets: (0..=1024u32).map(|c| c * 108).collect(),
        };
        for xorb in &xorbs {
            state
                .index
                .put_xorb(xorb, 1024 * 108, &table, &[], "s")
                .await
                .unwrap();
        }
        // Enough terms for three writers, alternating xorbs so none merge.
        let terms: Vec<Term> = (0..40_000u32)
            .map(|i| Term {
                xorb: xorbs[(i % 2) as usize],
                chunk_start: (i / 2) % 1024,
                chunk_end: (i / 2) % 1024 + 1,
                unpacked_bytes: 100,
            })
            .collect();
        let file = FileRecord {
            hash: openxet_protocol::chunk_hash(b"large"),
            size: 40_000 * 100,
            terms,
        };
        let new = NewFile {
            record: file.clone(),
            sha256: None,
            first_chunk: None,
        };
        state
            .index
            .register_shard(&xorbs[0], &[new], "s", Visibility::Global)
            .await
            .unwrap();
        let get = |path: String| {
            router(state.clone()).oneshot(
                Request::get(path)
                    .header(AUTHORIZATION, format!("{BEARER} {READ}"))
                    .body(Body::empty())
                    .unwrap(),
            )
        };
        let paths = [
            format!("/v2/reconstructions/{}", hash_to_hex(&file.hash)),
            format!("/v1/reconstructions/{}", hash_to_hex(&file.hash)),
            format!("/v1/reconstructions?file_id={}", hash_to_hex(&file.hash)),
        ];
        for path in paths {
            let threaded = get(path.clone()).await.unwrap();
            assert_eq!(threaded.status(), StatusCode::OK);
            let threaded = axum::body::to_bytes(threaded.into_body(), usize::MAX)
                .await
                .unwrap();
            // All but the request's own permit held: one writer.
            let held = Arc::clone(&state.validate)
                .acquire_many_owned(7)
                .await
                .unwrap();
            let single = get(path.clone()).await.unwrap();
            assert_eq!(single.status(), StatusCode::OK);
            let single = axum::body::to_bytes(single.into_body(), usize::MAX)
                .await
                .unwrap();
            drop(held);
            assert!(threaded == single, "{path}: the bytes differ");
        }
    }

    fn range(value: &str) -> Result<Option<ByteRange>, CasError> {
        let mut headers = HeaderMap::new();
        headers.insert(RANGE, HeaderValue::from_str(value).unwrap());
        requested_range(&headers)
    }

    #[test]
    fn range_headers_parse_as_one_byte_range() {
        let from = |start, end| Some(ByteRange::From { start, end });
        assert_eq!(
            range("bytes=0-255999999").unwrap(),
            from(0, Some(255_999_999))
        );
        assert_eq!(range("bytes=7-7").unwrap(), from(7, Some(7)));
        assert_eq!(range("Bytes= 5-").unwrap(), from(5, None));
        assert_eq!(range("bytes=-10").unwrap(), Some(ByteRange::Suffix(10)));
        assert_eq!(requested_range(&HeaderMap::new()).unwrap(), None);
        for bad in [
            "bytes=5-2",
            "bytes=-",
            "bytes=+1-2",
            "bytes=1-2,4-5",
            "bytes 1-2",
            "items=1-2",
            "bytes=18446744073709551616-",
        ] {
            assert!(
                matches!(range(bad), Err(CasError::InvalidRange)),
                "{bad} parsed"
            );
        }
        let mut two = HeaderMap::new();
        two.append(RANGE, HeaderValue::from_static("bytes=0-1"));
        two.append(RANGE, HeaderValue::from_static("bytes=0-1"));
        assert!(matches!(requested_range(&two), Err(CasError::InvalidRange)));
    }

    #[test]
    fn suffix_ranges_resolve_against_the_size() {
        assert_eq!(
            resolve(Some(ByteRange::Suffix(4)), 10).unwrap(),
            Some((6, u64::MAX))
        );
        assert_eq!(
            resolve(Some(ByteRange::Suffix(40)), 10).unwrap(),
            Some((0, u64::MAX))
        );
        for (len, size) in [(0, 10), (1, 0)] {
            assert!(matches!(
                resolve(Some(ByteRange::Suffix(len)), size),
                Err(CasError::RangeNotSatisfiable { .. })
            ));
        }
    }
}
