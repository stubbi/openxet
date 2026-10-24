//! Large synthetic files for the reconstruction latency gate
//! (`tests/latency_gate.rs`) and benchmark (`benches/reconstruction.rs`).
//!
//! A fixture holds two files of `terms` terms over `terms / 100` xorbs of
//! 1,024 chunks each, registered in a SQLite index on disk:
//! - [`Shape::Unmerged`], the worst case: consecutive terms name different
//!   xorbs, so no two share a fetch descriptor and a full answer signs one
//!   URL per term
//! - [`Shape::Merged`]: each xorb's 100 terms are adjacent runs of 10
//!   chunks, so a full answer has one descriptor per xorb
//!
//! Only the index is filled: reconstruction never reads the bucket.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::http::Request;
use axum::http::header::{AUTHORIZATION, RANGE};
use openxet_auth::{Authenticator, Mode, StaticToken, UrlSigner};
use openxet_cas::{AppState, BodyDeadline, CasConfig, MemoryBudget, SendDeadline, router};
use openxet_index::{ChunkTable, FileRecord, Index as _, NewFile, SqliteIndex, Term, Visibility};
use openxet_protocol::{MerkleHash, VerifyLimits, hash_to_hex};
use openxet_storage::LocalBucket;
use tempfile::TempDir;
use tower::ServiceExt as _;
use url::Url;

/// Terms per xorb in both shapes.
pub const TERMS_PER_XORB: u32 = 100;
const CHUNKS_PER_XORB: u32 = 1_024;
/// Every chunk's uncompressed length, and its stored length with the
/// 8-byte header: about a 64 MiB xorb, the largest a client builds.
const CHUNK_BYTES: u32 = 64 * 1024 - 64;
const STORED_CHUNK_BYTES: u32 = CHUNK_BYTES + 8;
/// Chunks between the starts of two terms of one xorb.
const CHUNK_STRIDE: u32 = 10;

const SCOPE: &str = "bench/large-files";
const READ: &str = "oxt_large_files_read";

/// A file's shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// One descriptor per term.
    Unmerged,
    /// One descriptor per xorb.
    Merged,
}

impl Shape {
    /// Both shapes, worst first.
    pub const ALL: [Self; 2] = [Self::Unmerged, Self::Merged];

    /// The shape's name in reports.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unmerged => "unmerged",
            Self::Merged => "merged",
        }
    }

    /// How many descriptors a full answer for a file of `terms` terms has.
    pub fn descriptors(self, terms: u32) -> usize {
        match self {
            Self::Unmerged => terms as usize,
            Self::Merged => xorbs(terms) as usize,
        }
    }
}

/// A server state over an index holding one file of each [`Shape`].
pub struct Fixture {
    /// The CAS state to route requests with.
    pub state: AppState,
    /// Each shape's file.
    pub files: [(Shape, FileRecord); 2],
    _dir: TempDir,
}

fn xorbs(terms: u32) -> u32 {
    terms.div_ceil(TERMS_PER_XORB)
}

/// A distinct hash per `(kind, a, b)`, without hashing.
fn synthetic(kind: u8, a: u32, b: u32) -> MerkleHash {
    let mut raw = [0u8; 32];
    raw[0] = kind;
    raw[1..5].copy_from_slice(&a.to_le_bytes());
    raw[5..9].copy_from_slice(&b.to_le_bytes());
    raw[31] = 0x5a;
    MerkleHash::from(raw)
}

fn xorb(i: u32) -> MerkleHash {
    synthetic(1, i, 0)
}

fn table(x: u32) -> ChunkTable {
    ChunkTable {
        hashes: (0..CHUNKS_PER_XORB)
            .map(|c| (synthetic(2, x, c), u64::from(CHUNK_BYTES)))
            .collect(),
        offsets: (0..=CHUNKS_PER_XORB)
            .map(|c| c * STORED_CHUNK_BYTES)
            .collect(),
    }
}

/// The file of `count` terms whose term `i` is `term(i)`: `(xorb, first
/// chunk, chunk count)`.
fn file(id: u32, count: u32, term: impl Fn(u32) -> (u32, u32, u32)) -> FileRecord {
    let terms: Vec<Term> = (0..count)
        .map(|i| {
            let (x, start, chunks) = term(i);
            Term {
                xorb: xorb(x),
                chunk_start: start,
                chunk_end: start + chunks,
                unpacked_bytes: chunks * CHUNK_BYTES,
            }
        })
        .collect();
    FileRecord {
        hash: synthetic(3, id, count),
        size: terms.iter().map(|t| u64::from(t.unpacked_bytes)).sum(),
        terms,
    }
}

fn shaped(shape: Shape, terms: u32) -> FileRecord {
    let n = xorbs(terms);
    match shape {
        Shape::Unmerged => file(0, terms, |i| (i % n, (i / n) * CHUNK_STRIDE, 1)),
        Shape::Merged => file(1, terms, |i| {
            (
                i / TERMS_PER_XORB,
                (i % TERMS_PER_XORB) * CHUNK_STRIDE,
                CHUNK_STRIDE,
            )
        }),
    }
}

fn state(dir: &Path, index: SqliteIndex) -> AppState {
    let token = StaticToken {
        sha256: StaticToken::digest(READ),
        scope: SCOPE.to_owned(),
        mode: Mode::Read,
        subject: "large-files".to_owned(),
    };
    AppState::new(
        Arc::new(index),
        Arc::new(LocalBucket::open(&dir.join("bucket")).unwrap()),
        Arc::new(
            Authenticator::new(
                vec![token],
                vec![],
                Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
            )
            .unwrap(),
        ),
        Arc::new(UrlSigner::new([9; 32], UrlSigner::DEFAULT_LIFETIME).unwrap()),
        CasConfig {
            public_url: Url::parse("https://cas.example.com").unwrap(),
            // The costlier visibility check: every layout read also checks
            // the xorb's scope.
            visibility: Visibility::PerScope,
            verify_limits: VerifyLimits::default(),
            heartbeat_interval: CasConfig::DEFAULT_HEARTBEAT_INTERVAL,
            body_deadline: BodyDeadline::DEFAULT,
            send_deadline: SendDeadline::DEFAULT,
            memory_budget: MemoryBudget::DEFAULT_BYTES,
        },
    )
}

/// A fixture whose files have `terms` terms each, a multiple of
/// [`TERMS_PER_XORB`].
pub async fn fixture(terms: u32) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let index = SqliteIndex::open(&dir.path().join("index.sqlite"))
        .await
        .unwrap();
    let files = seed(&index, SCOPE, terms).await;
    Fixture {
        state: state(dir.path(), index),
        files,
        _dir: dir,
    }
}

/// Registers one file of each [`Shape`], of `terms` terms each (a multiple
/// of [`TERMS_PER_XORB`]), and their xorbs to `scope` in `index`.
pub async fn seed(index: &SqliteIndex, scope: &str, terms: u32) -> [(Shape, FileRecord); 2] {
    assert!(terms.is_multiple_of(TERMS_PER_XORB));
    for x in 0..xorbs(terms) {
        let table = table(x);
        let size = u64::from(*table.offsets.last().unwrap()) + 1024;
        index
            .put_xorb(&xorb(x), size, &table, &[], scope)
            .await
            .unwrap();
    }
    let files = Shape::ALL.map(|shape| (shape, shaped(shape, terms)));
    let new: Vec<NewFile> = files
        .iter()
        .map(|(_, record)| NewFile {
            record: record.clone(),
            sha256: None,
            first_chunk: None,
        })
        .collect();
    index
        .register_shard(&synthetic(4, terms, 0), &new, scope, Visibility::PerScope)
        .await
        .unwrap();
    files
}

/// One `GET /v2/reconstructions/{file}` with `range` (inclusive) as its
/// `Range`, timed from the request to the body's last byte.
pub async fn get(
    state: &AppState,
    file: &MerkleHash,
    range: Option<(u64, u64)>,
) -> (Duration, Vec<u8>) {
    let mut request = Request::get(format!("/v2/reconstructions/{}", hash_to_hex(file)))
        .header(AUTHORIZATION, format!("{} {READ}", "Bearer"));
    if let Some((start, end)) = range {
        request = request.header(RANGE, format!("bytes={start}-{end}"));
    }
    let request = request.body(Body::empty()).unwrap();
    let started = Instant::now();
    let response = router(state.clone()).oneshot(request).await.unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let took = started.elapsed();
    assert_eq!(
        status,
        200,
        "{}",
        String::from_utf8_lossy(&body[..body.len().min(500)])
    );
    (took, body.to_vec())
}

/// How many signed URLs `body` holds.
pub fn urls(body: &[u8]) -> usize {
    body.windows(6).filter(|w| w == b"\"url\":").count()
}
