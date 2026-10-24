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
//! (ADR 0002). A `validate` permit is held from loading the chunk tables to
//! the serialized answer, and planning, signing and serialization run on a
//! blocking thread, as other CPU-bound request work does (rulings M1-R8,
//! M1-R25): a file of 100k terms signs 100k URLs. A batch is capped at
//! [`MAX_BATCH_FILES`] files and [`MAX_BATCH_TERMS`] terms.
//!
//! Every answer, errors included, is `Cache-Control: no-store`: it depends
//! on the caller's scope and carries URLs that act as credentials (design
//! §5.9 req 3).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequestParts, RawQuery, Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, RANGE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, head, post};
use openxet_index::ChunkTable;
use openxet_protocol::{MerkleHash, hash_to_hex};
use serde::ser::{SerializeStruct as _, Serializer};
use serde::{Deserialize, Serialize};
use tokio::sync::OwnedSemaphorePermit;
use url::Url;

use crate::blocking::blocking;
use crate::cache::private;
use crate::plan::{self, Fetch, Plan, PlanError, PlannedTerm};
use crate::{AppState, CasError, ReadAccess, body, path};

/// The most distinct files one batch reconstruction may name; a batch over
/// it is `400 batch_too_large`. Every file in a batch is planned whole, so
/// this bounds the work and the answer of one request.
pub const MAX_BATCH_FILES: usize = 1000;

/// The most terms one batch reconstruction may plan, over all the files it
/// is answered for; a batch over it is `400 batch_too_large`. With
/// [`MAX_BATCH_FILES`], it bounds one batch's chunk tables, signing and
/// answer, whatever the files hold.
pub const MAX_BATCH_TERMS: usize = 1_000_000;

// `CasError::BatchTooLarge`'s hint names the cap.
const _: () = assert!(MAX_BATCH_FILES == 1000);

/// The largest batch body read, in bytes: [`MAX_BATCH_FILES`] keys of about
/// 100 bytes each as the spec writes them, with room for whitespace.
const BATCH_BODY_LIMIT: u64 = 256 * 1024;

/// The reconstruction routes and `HEAD /v1/files/{hash}`.
pub(crate) fn routes() -> Router<AppState> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "256 KiB fits the usize of every target"
    )]
    let batch_limit = DefaultBodyLimit::max(BATCH_BODY_LIMIT as usize);
    Router::new()
        .route("/v2/reconstructions/{hash}", private(get(v2)))
        .route("/v1/reconstructions/{hash}", private(get(v1)))
        .route(
            "/v1/reconstructions",
            private(post(batch_post).layer(batch_limit).get(batch_get)),
        )
        .route("/v1/files/{hash}", private(head(head_file)))
}

/// The file named by the path ([`path::hash`]).
struct FilePath(MerkleHash);

impl FromRequestParts<AppState> for FilePath {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        path::hash(parts, state).await.map(Self)
    }
}

/// Which answer shape a single-file reconstruction takes (S10 §1).
#[derive(Debug, Clone, Copy)]
enum Version {
    /// `xorbs`: a list of `{url, ranges: [descriptor]}` per xorb.
    V2,
    /// `fetch_info`: a list of `{range, url, url_range}` per xorb.
    V1,
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
/// - `500` if the index's record disagrees with its chunk tables, never a
///   partial plan
async fn single(
    state: &AppState,
    scope: &str,
    file: MerkleHash,
    headers: &HeaderMap,
    version: Version,
) -> Result<Response, CasError> {
    let requested = requested_range(headers)?;
    let record = state
        .index
        .file(&file, scope)
        .await?
        .ok_or(CasError::FileNotFound)?;
    let range = resolve(requested, record.size)?;
    let window = plan::window(&record, range).map_err(planning_failed)?;
    let xorbs = window.map_or_else(Vec::new, |w| distinct_xorbs(&record.terms[w.terms]));
    let permit = validation_permit(state).await?;
    let tables = chunk_tables(state, scope, xorbs).await?;

    let signer = Signer::new(state);
    let body = render(permit, move || {
        let plan = plan::plan(&record, &tables, range).map_err(planning_failed)?;
        let mut catalog = Catalog::default();
        catalog.add(&plan.fetches);
        catalog.sign(&signer)?;
        let answer = Single {
            plan: &plan,
            catalog: &catalog,
            version,
        };
        serde_json::to_vec(&answer).map_err(CasError::internal)
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
    let mut records = Vec::new();
    for file in &files {
        if let Some(record) = state.index.file(file, scope).await? {
            records.push(record);
        }
    }
    let terms: usize = records.iter().map(|r| r.terms.len()).sum();
    if terms > MAX_BATCH_TERMS {
        return Err(CasError::BatchTooManyTerms {
            terms,
            limit: MAX_BATCH_TERMS,
        });
    }
    let xorbs = distinct_xorbs(records.iter().flat_map(|r| &r.terms));
    let permit = validation_permit(state).await?;
    let tables = chunk_tables(state, scope, xorbs).await?;

    let signer = Signer::new(state);
    let body = render(permit, move || {
        let plans = records
            .iter()
            .map(|record| {
                plan::plan(record, &tables, None)
                    .map(|plan| (record.hash, plan))
                    .map_err(planning_failed)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut catalog = Catalog::default();
        for (_, plan) in &plans {
            catalog.add(&plan.fetches);
        }
        catalog.sign(&signer)?;
        serde_json::to_vec(&Batch {
            plans: &plans,
            catalog: &catalog,
        })
        .map_err(CasError::internal)
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
enum ByteRange {
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
fn requested_range(headers: &HeaderMap) -> Result<Option<ByteRange>, CasError> {
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
fn resolve(range: Option<ByteRange>, size: u64) -> Result<Option<(u64, u64)>, CasError> {
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
    let mut seen = HashSet::new();
    terms
        .into_iter()
        .map(|t| t.xorb)
        .filter(|x| seen.insert(*x))
        .collect()
}

/// The chunk tables of `xorbs`, every one of which a file registered to
/// `scope` names. Registration checked each was visible to `scope` (ruling
/// M1-R17), so a missing one is an index fault: `500`, logged as an
/// integrity anomaly.
async fn chunk_tables(
    state: &AppState,
    scope: &str,
    xorbs: Vec<MerkleHash>,
) -> Result<HashMap<MerkleHash, ChunkTable>, CasError> {
    let tables = state
        .index
        .chunk_tables(&xorbs, scope, state.cfg.visibility)
        .await?;
    if let Some(missing) = xorbs.iter().find(|x| !tables.contains_key(x)) {
        return Err(CasError::internal(format!(
            "integrity anomaly: a registered file names xorb {}, whose chunk table the index \
             doesn't hold",
            hash_to_hex(missing)
        )));
    }
    Ok(tables)
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

/// A `validate` permit, taken before a plan's chunk tables are loaded and
/// held until its answer is serialized: the tables are most of a large
/// plan's memory, and planning, signing and serializing its CPU (rulings
/// M1-R8, M1-R25).
async fn validation_permit(state: &AppState) -> Result<OwnedSemaphorePermit, CasError> {
    Arc::clone(&state.validate)
        .acquire_owned()
        .await
        .map_err(CasError::internal)
}

/// Runs `work` on a blocking thread holding `permit` ([`blocking`]).
async fn render(
    permit: OwnedSemaphorePermit,
    work: impl FnOnce() -> Result<Vec<u8>, CasError> + Send + 'static,
) -> Result<Vec<u8>, CasError> {
    blocking(permit, work).await?.1
}

fn json(body: Vec<u8>) -> Response {
    ([(CONTENT_TYPE, "application/json")], body).into_response()
}

/// What signing a transfer URL needs, owned for a blocking thread.
struct Signer {
    urls: Arc<openxet_auth::UrlSigner>,
    base: Url,
    now: u64,
}

impl Signer {
    fn new(state: &AppState) -> Self {
        Self {
            urls: Arc::clone(&state.urls),
            base: state.cfg.public_url.clone(),
            now: state.clock.now(),
        }
    }
}

/// The fetch descriptors of one answer, each signed once, grouped by xorb.
/// A descriptor two terms or two files share is listed once.
#[derive(Default)]
struct Catalog {
    /// The distinct xorbs, in order of first use, and their hex names.
    xorbs: Vec<(MerkleHash, String)>,
    /// Where each xorb is in `xorbs`.
    index: HashMap<MerkleHash, usize>,
    /// Per entry of `xorbs`, its descriptors' indices in `fetches`.
    groups: Vec<Vec<usize>>,
    fetches: Vec<Fetch>,
    /// The signed URL of each of `fetches`.
    urls: Vec<Url>,
    seen: HashSet<(MerkleHash, (u32, u32))>,
}

impl Catalog {
    fn add(&mut self, fetches: &[Fetch]) {
        for fetch in fetches {
            if !self.seen.insert((fetch.xorb, fetch.chunks)) {
                continue;
            }
            let xorb = self.xorb(&fetch.xorb);
            self.groups[xorb].push(self.fetches.len());
            self.fetches.push(*fetch);
        }
    }

    fn xorb(&mut self, xorb: &MerkleHash) -> usize {
        *self.index.entry(*xorb).or_insert_with(|| {
            self.xorbs.push((*xorb, hash_to_hex(xorb)));
            self.groups.push(Vec::new());
            self.xorbs.len() - 1
        })
    }

    /// The hex name of `xorb`, which a term of an added plan names.
    fn name(&self, xorb: &MerkleHash) -> &str {
        &self.xorbs[self.index[xorb]].1
    }

    /// Signs one URL per descriptor, for exactly its byte range (ADR 0002).
    fn sign(&mut self, signer: &Signer) -> Result<(), CasError> {
        self.urls = self
            .fetches
            .iter()
            .map(|f| {
                signer
                    .urls
                    .sign_xorb_range(&signer.base, &f.xorb, f.bytes.0, f.bytes.1, signer.now)
            })
            .collect::<Result<_, _>>()?;
        Ok(())
    }
}

/// A `{start, end}` range: chunk indices (end-exclusive) or bytes
/// (end-inclusive), as S10 §1 types each.
#[derive(Serialize)]
struct Span<T> {
    start: T,
    end: T,
}

/// One term: `{hash, unpacked_length, range}`.
#[derive(Serialize)]
struct TermJson<'a> {
    hash: &'a str,
    unpacked_length: u32,
    range: Span<u32>,
}

/// One v2 descriptor: `{chunks, bytes}`.
#[derive(Serialize)]
struct Descriptor {
    chunks: Span<u32>,
    bytes: Span<u64>,
}

/// One v2 fetch entry, always of a single descriptor (ADR 0002 §1).
#[derive(Serialize)]
struct EntryV2<'a> {
    url: &'a str,
    ranges: [Descriptor; 1],
}

/// One v1 `fetch_info` entry.
#[derive(Serialize)]
struct EntryV1<'a> {
    range: Span<u32>,
    url: &'a str,
    url_range: Span<u64>,
}

/// A plan's terms, as a JSON list.
struct Terms<'a> {
    terms: &'a [PlannedTerm],
    catalog: &'a Catalog,
}

impl Serialize for Terms<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(self.terms.iter().map(|t| TermJson {
            hash: self.catalog.name(&t.xorb),
            unpacked_length: t.unpacked_length,
            range: Span {
                start: t.chunks.0,
                end: t.chunks.1,
            },
        }))
    }
}

/// The catalog's entries per xorb: v2's `xorbs` or v1's `fetch_info`.
struct Entries<'a> {
    catalog: &'a Catalog,
    version: Version,
}

impl Serialize for Entries<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let catalog = self.catalog;
        s.collect_map(
            catalog
                .xorbs
                .iter()
                .zip(&catalog.groups)
                .map(|((_, name), fetches)| {
                    (
                        name,
                        Group {
                            catalog,
                            fetches,
                            version: self.version,
                        },
                    )
                }),
        )
    }
}

/// One xorb's entries.
struct Group<'a> {
    catalog: &'a Catalog,
    fetches: &'a [usize],
    version: Version,
}

impl Serialize for Group<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let spans = |i: usize| {
            let fetch = &self.catalog.fetches[i];
            (
                self.catalog.urls[i].as_str(),
                Span {
                    start: fetch.chunks.0,
                    end: fetch.chunks.1,
                },
                Span {
                    start: fetch.bytes.0,
                    end: fetch.bytes.1,
                },
            )
        };
        let entries = self.fetches.iter().map(|&i| spans(i));
        match self.version {
            Version::V2 => s.collect_seq(entries.map(|(url, chunks, bytes)| EntryV2 {
                url,
                ranges: [Descriptor { chunks, bytes }],
            })),
            Version::V1 => s.collect_seq(entries.map(|(url, range, url_range)| EntryV1 {
                range,
                url,
                url_range,
            })),
        }
    }
}

/// A single-file answer: v2's `{offset_into_first_range, terms, xorbs}` or
/// v1's `{offset_into_first_range, terms, fetch_info}` (S10 §1).
struct Single<'a> {
    plan: &'a Plan,
    catalog: &'a Catalog,
    version: Version,
}

impl Serialize for Single<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut out = s.serialize_struct("Reconstruction", 3)?;
        out.serialize_field(
            "offset_into_first_range",
            &self.plan.offset_into_first_range,
        )?;
        out.serialize_field(
            "terms",
            &Terms {
                terms: &self.plan.terms,
                catalog: self.catalog,
            },
        )?;
        let entries = Entries {
            catalog: self.catalog,
            version: self.version,
        };
        match self.version {
            Version::V2 => out.serialize_field("xorbs", &entries)?,
            Version::V1 => out.serialize_field("fetch_info", &entries)?,
        }
        out.end()
    }
}

/// A batch answer: `{files, fetch_info}` (S10 §1).
struct Batch<'a> {
    plans: &'a [(MerkleHash, Plan)],
    catalog: &'a Catalog,
}

impl Serialize for Batch<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        struct Files<'a>(&'a Batch<'a>);
        impl Serialize for Files<'_> {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_map(self.0.plans.iter().map(|(file, plan)| {
                    (
                        hash_to_hex(file),
                        Terms {
                            terms: &plan.terms,
                            catalog: self.0.catalog,
                        },
                    )
                }))
            }
        }
        let mut out = s.serialize_struct("BatchReconstruction", 2)?;
        out.serialize_field("files", &Files(self))?;
        out.serialize_field(
            "fetch_info",
            &Entries {
                catalog: self.catalog,
                version: Version::V1,
            },
        )?;
        out.end()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::HeaderValue;
    use axum::http::header::AUTHORIZATION;
    use openxet_auth::{Mode, StaticToken};
    use openxet_index::{FileRecord, Index, IndexError, NewFile, Term, Visibility};
    use tokio::sync::Semaphore;
    use tower::ServiceExt as _;

    use super::*;
    use crate::{router, test_support};

    // The scheme is a constant so the repository's secret scanner, which
    // flags any literal bearer value, can tell this test token from leaks.
    const BEARER: &str = "Bearer";
    const READ: &str = "oxt_unit_read";

    /// An index counting its `chunk_tables` calls.
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

        async fn dedup_xorb(
            &self,
            chunk: &MerkleHash,
            scope: &str,
            vis: Visibility,
        ) -> Result<Option<MerkleHash>, IndexError> {
            self.inner.dedup_xorb(chunk, scope, vis).await
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
