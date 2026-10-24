//! `POST /v1/shards` and `POST /v2/shards` (S10 §4; spec
//! <https://huggingface.co/docs/xet/api#4-upload-shard> and
//! <https://huggingface.co/docs/xet/api#5-streaming-shard-upload>).
//!
//! Both versions run the same ingest:
//! 1. parse the body (`read_upload_shard`) on a blocking thread
//! 2. check the verification limits, before loading anything
//! 3. load the chunk tables of the referenced xorbs visible to the caller
//! 4. verify the shard against them, then build its stored form and hash,
//!    on a blocking thread
//! 5. store the shard in the bucket, then register its files in the index,
//!    holding a `commit` permit (ruling M1-R24)
//!
//! Bucket before index (design §5.3): the index never names a shard the
//! bucket lacks. Ingest is idempotent, since `hf_xet` resends the whole
//! shard after a lost answer or a retryable failure (S10 §4): a repeat
//! stores the same bytes under the same hash and registers nothing new.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, FromRequestParts as _, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use http_body_util::channel::{Channel, Sender};
use openxet_index::{FileRecord, NewFile, Term};
use openxet_protocol::{
    MAX_UPLOAD_SHARD_BYTES, MerkleHash, ProtocolError, UploadShard, VerifyError, hash_to_hex,
    read_upload_shard, shard_hash, stored_shard, verify_shard,
};
use openxet_storage::Key;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::Instrument as _;

use crate::blocking::blocking;
use crate::{AppState, CasError, RequestId, WriteAccess, body};

/// [`MAX_UPLOAD_SHARD_BYTES`] as a buffer length.
#[allow(
    clippy::cast_possible_truncation,
    reason = "64 MiB fits the usize of every target with a 32-bit or wider address space"
)]
const BODY_LIMIT: usize = MAX_UPLOAD_SHARD_BYTES as usize;

/// Event lines buffered for a client that reads slowly. The work never
/// waits on the client: progress reaches the stream over an unbounded
/// channel, and only the stream task waits here.
const EVENT_BUFFER: usize = 16;

/// A chunk table as `verify_shard` and `stored_shard` look it up.
type Lookup = HashMap<MerkleHash, Arc<[(MerkleHash, u64)]>>;

/// `POST /v1/shards` and `POST /v2/shards` (S10 §4).
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/shards",
            post(upload_v1).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
        .route(
            "/v2/shards",
            post(upload_v2).layer(DefaultBodyLimit::max(BODY_LIMIT)),
        )
}

/// The v1 answer. `hf_xet` decodes `result` as a `u8` enum, `0` "exists"
/// and `1` "registered", and retries any other body as a decode error
/// (S10 §4).
#[derive(Serialize)]
struct Registered {
    /// `1` if the shard or one of its files was new to the caller's scope.
    result: u8,
}

/// `POST /v1/shards`: `200 {"result": 1|0}`. A shard that doesn't parse or
/// verify is `400` with its rule's code, final; a storage or index failure
/// is `503`, which the client retries. The client doesn't show the body, so
/// the full reason is logged at warn under the request id (S10 §4;
/// [`CasError::log`]).
async fn upload_v1(
    State(state): State<AppState>,
    request: Request,
) -> Result<Json<Registered>, Response> {
    let (scope, shard) = accept(&state, request).await?;
    let inserted = ingest(&state, &scope, shard, &Progress::none())
        .await
        .map_err(IntoResponse::into_response)?;
    Ok(Json(Registered {
        result: u8::from(inserted),
    }))
}

/// `POST /v2/shards`: once the caller is authorized and the body read
/// within the limit, `200` and a stream of newline-delimited JSON events
/// (S10 §4):
/// - `validating`, counting files, first before parsing (`0` of `0`), then
///   once the shard is parsed and once it is verified
/// - `committing` with stage `uploading` before the bucket write, then
///   `syncing` before the index registration
/// - exactly one terminal event, always last: `result` (`1` or `0`, as v1)
///   or `error`
///
/// While work runs, the last event repeats every `heartbeat_interval`, as
/// the spec asks: the client fails a stream silent for 300 s.
///
/// Every failure after the `200` is an `error` event, never a status: a
/// `404` or `501` here would send the client to v1 with the same shard, and
/// any status would lose the message, the only server text the user sees.
/// A shard that doesn't parse or verify gets `retryable: false` and the
/// rule it broke, naming only what the client sent (ruling M1-R23). A storage or index failure gets `retryable: true`, which
/// retries the whole upload, and a message naming the request id; its
/// cause is logged under that id.
///
/// The work runs in its own task, which the stream doesn't own: a client
/// that disconnects mid-stream doesn't abort it. It finishes, so a shard is
/// never left stored with its files half-registered, and the client's retry
/// finds it registered and gets `result: 0`.
async fn upload_v2(State(state): State<AppState>, request: Request) -> Result<Response, Response> {
    let request_id = request.extensions().get::<RequestId>().copied();
    let (scope, shard) = accept(&state, request).await?;
    let (progress, events) = mpsc::unbounded_channel();
    let span = tracing::Span::current();
    let work = tokio::spawn({
        let state = state.clone();
        async move {
            let outcome = ingest(&state, &scope, shard, &Progress(Some(progress))).await;
            if let Err(e) = &outcome {
                e.log();
            }
            outcome
        }
        .instrument(span.clone())
    });
    let (lines, body) = Channel::<Bytes, Infallible>::new(EVENT_BUFFER);
    tokio::spawn(
        stream(
            work,
            events,
            lines,
            state.cfg.heartbeat_interval,
            request_id,
        )
        .instrument(span),
    );
    Ok((
        [
            (CONTENT_TYPE, "application/x-ndjson"),
            (CACHE_CONTROL, "no-cache"),
        ],
        Body::new(body),
    )
        .into_response())
}

/// The caller's scope and the shard body: the credential first, then the
/// body within [`MAX_UPLOAD_SHARD_BYTES`]. A refusal is a status on both
/// versions, final for the client (S10 §4).
async fn accept(state: &AppState, request: Request) -> Result<(String, Bytes), Response> {
    let (mut parts, body) = request.into_parts();
    let principal = match WriteAccess::from_request_parts(&mut parts, state).await {
        Ok(WriteAccess(principal)) => principal,
        Err(e) => {
            return Err(body::refuse_unread(e, &parts.headers, body, MAX_UPLOAD_SHARD_BYTES).await);
        }
    };
    let shard = body::buffer(
        Request::from_parts(parts, body),
        MAX_UPLOAD_SHARD_BYTES,
        too_large,
    )
    .await?;
    Ok((principal.scope, shard))
}

fn too_large() -> CasError {
    CasError::ShardTooLarge {
        limit: MAX_UPLOAD_SHARD_BYTES,
    }
}

/// A `/v2/shards` event (S10 §4; `hf_xet` 1.7.0's `ShardUploadEvent`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Event {
    /// Verification progress, in files.
    Validating { verified: u64, total: u64 },
    /// The durable write.
    Committing { stage: Stage },
    /// Terminal success. `result` is v1's value; the client accepts the
    /// event with or without it, and the spec sends it.
    Result { result: u8 },
    /// Terminal failure.
    Error { message: String, retryable: bool },
}

/// A step of the durable write.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Stage {
    /// Storing the shard in the bucket.
    Uploading,
    /// Registering its files in the index.
    Syncing,
}

/// Where ingest reports progress: the v2 stream, or nowhere for v1.
struct Progress(Option<mpsc::UnboundedSender<Event>>);

impl Progress {
    fn none() -> Self {
        Self(None)
    }

    fn report(&self, event: Event) {
        if let Some(events) = &self.0 {
            // Fails only once the stream has ended because its client left;
            // the work goes on regardless (see `upload_v2`).
            drop(events.send(event));
        }
    }
}

/// Writes the v2 stream: each progress event as it comes, the last one
/// again after `heartbeat` of silence, and the outcome of `work` as the one
/// terminal event. Every event the work reported goes out before the
/// terminal one: those still queued when the work ends are drained first.
async fn stream(
    mut work: JoinHandle<Result<bool, CasError>>,
    mut events: mpsc::UnboundedReceiver<Event>,
    mut lines: Sender<Bytes>,
    heartbeat: Duration,
    request_id: Option<RequestId>,
) {
    let mut last = Event::Validating {
        verified: 0,
        total: 0,
    };
    let mut connected = send(&mut lines, &last).await;
    let outcome = loop {
        tokio::select! {
            biased;
            Some(event) = events.recv() => {
                last = event;
                connected = connected && send(&mut lines, &last).await;
            }
            outcome = &mut work => break outcome,
            () = tokio::time::sleep(heartbeat) => {
                connected = connected && send(&mut lines, &last).await;
            }
        }
        if !connected {
            // The work task is detached, not aborted, when its handle drops:
            // it finishes on its own (see `upload_v2`).
            tracing::info!("client left the shard stream; the upload continues");
            return;
        }
    };
    // The work may have queued its last events after the select last
    // polled `events`; they go out before the terminal event.
    while let Ok(event) = events.try_recv() {
        connected = connected && send(&mut lines, &event).await;
    }
    let terminal = match outcome {
        Ok(Ok(inserted)) => Event::Result {
            result: u8::from(inserted),
        },
        Ok(Err(e)) => error_event(&e, request_id),
        Err(panicked) => {
            let e = CasError::internal(panicked);
            e.log();
            error_event(&e, request_id)
        }
    };
    send(&mut lines, &terminal).await;
}

/// Sends `event` as one line. Returns whether the client is still there.
async fn send(lines: &mut Sender<Bytes>, event: &Event) -> bool {
    let mut line = serde_json::to_vec(event)
        .unwrap_or_else(|_| unreachable!("an event has only string and integer fields"));
    line.push(b'\n');
    lines.send_data(Bytes::from(line)).await.is_ok()
}

/// The terminal `error` event for `e`. The request's own fault carries its
/// message, final. A server fault carries a generic message and the request
/// id, since the user sees nothing else, and is retryable when a retry may
/// help (S10 §4).
fn error_event(e: &CasError, request_id: Option<RequestId>) -> Event {
    if !e.is_server_error() {
        return Event::Error {
            message: e.to_string(),
            retryable: false,
        };
    }
    let message = match request_id {
        Some(id) => format!("{e}. If this persists, give the operator this request id: {id}"),
        None => e.to_string(),
    };
    Event::Error {
        message,
        retryable: e.is_retryable(),
    }
}

/// Parses, verifies, stores and registers the shard `body` for `scope`.
/// Returns whether anything was new to `scope`.
async fn ingest(
    state: &AppState,
    scope: &str,
    body: Bytes,
    progress: &Progress,
) -> Result<bool, CasError> {
    // One permit covers parsing, loading the chunk tables and verifying, so
    // at most `validate`'s permit count of shards are in those steps at
    // once. Each holds its body (at most `MAX_UPLOAD_SHARD_BYTES`), its
    // parsed form and at most `max_referenced_xorbs` chunk tables of at most
    // 320 KiB each (S6 "M1 index implication"), so that is what bounds the
    // process's verification memory, however many uploads wait (ruling
    // M1-R8). A v2 upload whose client left waits like any other.
    let permit = Arc::clone(&state.validate)
        .acquire_owned()
        .await
        .map_err(CasError::internal)?;
    let (permit, shard) = blocking(permit, move || read_upload_shard(&body[..])).await?;
    let shard = shard.map_err(shard_error)?;
    let total = u64::try_from(shard.files.len()).map_err(CasError::internal)?;
    progress.report(Event::Validating { verified: 0, total });

    // The limits bound the chunk tables loaded below, so they are checked
    // before any is.
    let limits = state.cfg.verify_limits;
    limits.check(&shard).map_err(verification_failed)?;
    let referenced: Vec<MerkleHash> = shard.referenced_xorbs().copied().collect();
    // A xorb missing from the answer is missing or not visible to `scope`;
    // verification reports both as the same unknown xorb (ruling M1-R17).
    let lookup: Lookup = state
        .index
        .chunk_tables(&referenced, scope, state.cfg.visibility)
        .await?
        .into_iter()
        .map(|(xorb, table)| (xorb, table.hashes))
        .collect();

    // The parsed shard and its chunk tables are dropped with the closure:
    // from here on the upload holds only the stored bytes and its files.
    let (permit, verified) = blocking(permit, move || {
        verify_shard(&shard, &lookup, limits).map_err(verification_failed)?;
        let stored = stored_shard(&shard, &lookup).map_err(shard_error)?;
        let hash = shard_hash(&stored);
        let files = new_files(&shard, &lookup)?;
        Ok::<_, CasError>((stored, hash, files))
    })
    .await?;
    drop(permit);
    let (stored, hash, files) = verified?;
    progress.report(Event::Validating {
        verified: total,
        total,
    });

    // Held from the bucket write until registration ends (ruling M1-R24):
    // it bounds how many uploads hold a stored shard and its files while
    // waiting on storage, a v2 upload whose client left included. The
    // validation permit is already returned, so slow storage never holds
    // a CPU permit.
    let _commit = Arc::clone(&state.commit)
        .acquire_owned()
        .await
        .map_err(CasError::internal)?;
    progress.report(Event::Committing {
        stage: Stage::Uploading,
    });
    state
        .bucket
        .put(&Key::Shard(hash), Bytes::from(stored))
        .await?;
    // The shard is durable from here. If registration fails, the client
    // retries the whole upload: the put repeats as a no-op and the
    // registration completes. A stored shard no index row names costs only
    // space until garbage collection; the reverse never happens.
    progress.report(Event::Committing {
        stage: Stage::Syncing,
    });
    let inserted = state
        .index
        .register_shard(&hash, &files, scope, state.cfg.visibility)
        .await?;
    tracing::info!(
        shard = %hash_to_hex(&hash),
        files = files.len(),
        inserted,
        "shard registered"
    );
    Ok(inserted)
}

/// The files of a verified shard as the index registers them: terms map
/// one to one from segments, the size is their unpacked total, and the
/// first chunk is the dedup candidate for the first term's xorb (S10 §2).
fn new_files(shard: &UploadShard, lookup: &Lookup) -> Result<Vec<NewFile>, CasError> {
    shard
        .files
        .iter()
        .map(|file| {
            let first_chunk = file
                .segments
                .first()
                .map(|seg| {
                    let chunk = usize::try_from(seg.chunk_start).ok();
                    lookup
                        .get(&seg.xorb)
                        .and_then(|table| table.get(chunk?))
                        .map(|(hash, _)| *hash)
                        .ok_or_else(|| {
                            CasError::internal("a verified segment starts outside its xorb's table")
                        })
                })
                .transpose()?;
            Ok(NewFile {
                record: FileRecord {
                    hash: file.file_hash,
                    size: file
                        .segments
                        .iter()
                        .map(|seg| u64::from(seg.unpacked_bytes))
                        .sum(),
                    terms: file
                        .segments
                        .iter()
                        .map(|seg| Term {
                            xorb: seg.xorb,
                            chunk_start: seg.chunk_start,
                            chunk_end: seg.chunk_end,
                            unpacked_bytes: seg.unpacked_bytes,
                        })
                        .collect(),
                },
                sha256: file.sha256,
                first_chunk,
            })
        })
        .collect()
}

/// A shard that failed verification. Every variant is named, so a new rule
/// is a compile error here rather than a misclassified answer. A referenced
/// xorb the caller can't see becomes `unknown_xorb`, with the message that
/// tells a user how to recover from a stale client cache (ruling M1-R5); the
/// rest name the file or xorb and the rule broken.
fn verification_failed(reason: VerifyError) -> CasError {
    match reason {
        VerifyError::UnknownXorb { xorb, .. } => CasError::UnknownXorb { xorb },
        VerifyError::OutOfBounds { .. }
        | VerifyError::MissingVerification { .. }
        | VerifyError::RangeHashMismatch { .. }
        | VerifyError::LengthMismatch { .. }
        | VerifyError::FileHashMismatch { .. }
        | VerifyError::EmptyFileSha256 { .. }
        | VerifyError::ConflictingFileSha256 { .. }
        | VerifyError::TooMuchWork { .. }
        | VerifyError::TooManyXorbs { .. } => CasError::ShardVerificationFailed(Box::new(reason)),
    }
}

/// A shard the protocol crate refused to parse or store. Only a parse
/// failure is the client's; the rest can't follow a passed verification, so
/// they are server bugs. Listed rather than matched by a wildcard, so a new
/// variant is a reviewed decision.
fn shard_error(reason: ProtocolError) -> CasError {
    match reason {
        ProtocolError::MalformedShard(_) => CasError::MalformedShard(Box::new(reason)),
        ProtocolError::ShardNotVerified(_)
        | ProtocolError::ShardFileSha256Conflict { .. }
        | ProtocolError::XorbTableTooLarge { .. }
        | ProtocolError::ShardSerialization(_)
        | ProtocolError::InvalidHex(_)
        | ProtocolError::MalformedXorb(_)
        | ProtocolError::XorbTooLarge { .. }
        | ProtocolError::XorbEmpty
        | ProtocolError::XorbTooManyChunks { .. }
        | ProtocolError::XorbChunkEmpty { .. }
        | ProtocolError::XorbChunkTooLong { .. }
        | ProtocolError::XorbChunkTooShort { .. }
        | ProtocolError::XorbTruncated { .. }
        | ProtocolError::XorbTrailingBytes { .. }
        | ProtocolError::DedupKeyZero => CasError::internal(reason),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::atomic::{AtomicUsize, Ordering};

    use openxet_index::ChunkTable;
    use openxet_storage::{Bucket, StorageError};
    use tokio::sync::Semaphore;

    use super::*;
    use crate::test_support::{self, PermitProbe};

    /// A bucket whose writes take 200 ms, recording the most writes in
    /// flight at once.
    struct SlowBucket {
        inner: Arc<dyn Bucket>,
        in_flight: AtomicUsize,
        most: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl Bucket for SlowBucket {
        async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError> {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.most.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(200)).await;
            let out = self.inner.put(key, bytes).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            out
        }

        async fn get_range(&self, key: &Key, start: u64, end: u64) -> Result<Bytes, StorageError> {
            self.inner.get_range(key, start, end).await
        }

        async fn get_range_stream(
            &self,
            key: &Key,
            start: u64,
            end: u64,
        ) -> Result<openxet_storage::RangeStream, StorageError> {
            self.inner.get_range_stream(key, start, end).await
        }

        async fn size(&self, key: &Key) -> Result<Option<u64>, StorageError> {
            self.inner.size(key).await
        }
    }

    fn capture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testdata/hf-xet-1.7.0-upload")
                .join(name),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn the_commit_permit_bounds_uploads_between_bucket_write_and_registration() {
        // Ruling M1-R24: with one commit permit, a second upload's bucket
        // write waits until the first has registered, so slow storage can't
        // pile up committing uploads (or their stored bytes) in memory.
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![]).await;
        state.commit = Arc::new(Semaphore::new(1));
        let bucket = Arc::new(SlowBucket {
            inner: Arc::clone(&state.bucket),
            in_flight: AtomicUsize::new(0),
            most: AtomicUsize::new(0),
        });
        state.bucket = Arc::clone(&bucket) as Arc<dyn Bucket>;
        let xorb = openxet_protocol::xorb_with_footer(&capture(
            "43dcdd6c959fc72d974ab1a08aeaa67de0d43fec6b5694598419af0be2f7040c.xorb",
        ))
        .unwrap();
        let table = ChunkTable {
            hashes: xorb.chunks.hashes.into(),
            offsets: xorb.chunk_offsets.into(),
        };
        for scope in ["a", "b"] {
            state
                .index
                .put_xorb(&xorb.hash, xorb.bytes.len() as u64, &table, &[], scope)
                .await
                .unwrap();
        }
        let shard = Bytes::from(capture("upload.shard"));

        let silent = Progress::none();
        let (a, b) = tokio::join!(
            ingest(&state, "a", shard.clone(), &silent),
            ingest(&state, "b", shard, &silent),
        );

        assert!(a.unwrap() && b.unwrap());
        assert_eq!(bucket.most.load(Ordering::SeqCst), 1);
        assert_eq!(state.commit.available_permits(), 1);
    }

    #[tokio::test]
    async fn one_permit_covers_loading_the_tables_and_verifying() {
        // The chunk tables are the large part of verification memory, so
        // they are loaded under the permit that verification then uses.
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_support::state(dir.path(), vec![]).await;
        state.validate = Arc::new(Semaphore::new(1));
        let probe = PermitProbe::install(&mut state);
        let shard = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../testdata/hf-xet-1.7.0-upload/upload.shard"),
        )
        .unwrap();

        // The capture's xorb isn't stored here, so verification fails; the
        // tables were loaded all the same.
        let outcome = ingest(&state, "s", Bytes::from(shard), &Progress::none()).await;

        assert!(
            matches!(outcome, Err(CasError::UnknownXorb { .. })),
            "{outcome:?}"
        );
        assert_eq!(probe.free_at_load(), Some(0));
        assert_eq!(state.validate.available_permits(), 1);
    }
}
