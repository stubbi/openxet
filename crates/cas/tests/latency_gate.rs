//! The reconstruction latency gate (design §5.8, §16 "Speed"):
//! reconstruction p99 < 50 ms for files up to 100k terms.
//!
//! The file shape is pinned, so the gate can't pass by measuring an easy
//! file. Both files have 100,000 terms over 1,000 xorbs of 1,024 chunks:
//! - **unmerged**, the worst case: consecutive terms name different xorbs,
//!   so no two share a fetch descriptor and a full answer signs 100,000 URLs
//! - **merged**: each xorb's 100 terms are adjacent runs, so a full answer
//!   has 1,000 descriptors
//!
//! Each shape is asked 200 full-file and 200 random-range
//! `GET /v2/reconstructions` in process, one at a time, through the whole
//! router and a SQLite index on disk. Latency runs from the request to the
//! last byte of the body. The gate holds in release builds only, as CI runs
//! it: `cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)'`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, to_bytes};
use axum::http::Request;
use axum::http::header::{AUTHORIZATION, RANGE};
use openxet_auth::{Authenticator, Mode, StaticToken, UrlSigner};
use openxet_cas::{AppState, BodyDeadline, CasConfig, SendDeadline, router};
use openxet_index::{ChunkTable, FileRecord, NewFile, SqliteIndex, Term, Visibility};
use openxet_protocol::{MerkleHash, VerifyLimits, hash_to_hex};
use openxet_storage::LocalBucket;
use tower::ServiceExt as _;
use url::Url;

const TERMS: u32 = 100_000;
const XORBS: u32 = 1_000;
const CHUNKS_PER_XORB: u32 = 1_024;
/// Every chunk's uncompressed length, and its stored length with the
/// 8-byte header: about a 64 MiB xorb, the largest a client builds.
const CHUNK_BYTES: u32 = 64 * 1024 - 64;
const STORED_CHUNK_BYTES: u32 = CHUNK_BYTES + 8;
/// Requests of each kind per shape.
const REQUESTS: usize = 200;
/// Design §5.8.
const P99_LIMIT: Duration = Duration::from_millis(50);

const SCOPE: &str = "bench/gate";
const READ: &str = "oxt_latency_gate_read";

/// xorshift64*, fixed seed: the same ranges on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
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

/// The file whose term `i` is `term(i)`: `(xorb, first chunk, chunk count)`.
fn file(id: u32, term: impl Fn(u32) -> (u32, u32, u32)) -> FileRecord {
    let terms: Vec<Term> = (0..TERMS)
        .map(|i| {
            let (x, start, count) = term(i);
            Term {
                xorb: xorb(x),
                chunk_start: start,
                chunk_end: start + count,
                unpacked_bytes: count * CHUNK_BYTES,
            }
        })
        .collect();
    FileRecord {
        hash: synthetic(3, id, 0),
        size: terms.iter().map(|t| u64::from(t.unpacked_bytes)).sum(),
        terms,
    }
}

/// Consecutive terms name different xorbs, one chunk each: 100,000
/// descriptors.
fn unmerged() -> FileRecord {
    file(0, |i| (i % XORBS, (i / XORBS) * 10, 1))
}

/// Each xorb's 100 terms in a row, 10 adjacent chunks each: 1,000
/// descriptors.
fn merged() -> FileRecord {
    let per_xorb = TERMS / XORBS;
    file(1, |i| (i / per_xorb, (i % per_xorb) * 10, 10))
}

async fn state(dir: &std::path::Path) -> AppState {
    let token = StaticToken {
        sha256: StaticToken::digest(READ),
        scope: SCOPE.to_owned(),
        mode: Mode::Read,
        subject: "latency-gate".to_owned(),
    };
    AppState::new(
        Arc::new(SqliteIndex::open(&dir.join("index.sqlite")).await.unwrap()),
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
            // The costlier visibility check: every table read also checks
            // the xorb's scope.
            visibility: Visibility::PerScope,
            verify_limits: VerifyLimits::default(),
            heartbeat_interval: CasConfig::DEFAULT_HEARTBEAT_INTERVAL,
            body_deadline: BodyDeadline::DEFAULT,
            dedup_send: SendDeadline::DEFAULT,
        },
    )
}

/// One `GET /v2/reconstructions/{file}`, timed to the body's last byte.
async fn get(
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

/// The p99 of `samples`: the smallest that at least 99% are at or under.
fn p99(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    samples[(samples.len() * 99).div_ceil(100) - 1]
}

fn millis(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    debug_assertions,
    ignore = "a latency gate holds in release builds: run it with --release"
)]
async fn reconstruction_p99_under_50ms_for_100k_terms() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    for x in 0..XORBS {
        let table = table(x);
        let size = u64::from(*table.offsets.last().unwrap()) + 1024;
        state
            .index
            .put_xorb(&xorb(x), size, &table, &[], SCOPE)
            .await
            .unwrap();
    }
    let shapes = [
        ("unmerged", unmerged(), 100_000),
        ("merged", merged(), 1_000),
    ];
    let files: Vec<NewFile> = shapes
        .iter()
        .map(|(_, record, _)| NewFile {
            record: record.clone(),
            sha256: None,
            first_chunk: None,
        })
        .collect();
    state
        .index
        .register_shard(&synthetic(4, 0, 0), &files, SCOPE, Visibility::PerScope)
        .await
        .unwrap();

    let mut rng = Rng(0x0e7a_11fe_9a7e_0001);
    let mut failures = Vec::new();
    for (name, record, descriptors) in &shapes {
        // The shape is what the gate claims: one URL per descriptor.
        let (_, body) = get(&state, &record.hash, None).await;
        let urls = body.windows(6).filter(|w| w == b"\"url\":").count();
        assert_eq!(urls, *descriptors, "{name}: the file's shape changed");

        let mut full = Vec::with_capacity(REQUESTS);
        let mut ranged = Vec::with_capacity(REQUESTS);
        let mut bytes = 0;
        for _ in 0..REQUESTS {
            let (took, body) = get(&state, &record.hash, None).await;
            full.push(took);
            bytes = body.len();
            let start = rng.below(record.size);
            let end = start + rng.below(record.size - start);
            ranged.push(get(&state, &record.hash, Some((start, end))).await.0);
        }
        for (kind, samples) in [("full", &mut full), ("range", &mut ranged)] {
            let p99 = p99(samples);
            let median = samples[samples.len() / 2];
            eprintln!(
                "{name} {kind}: p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms (full body {bytes} B)",
                millis(median),
                millis(p99),
                millis(*samples.last().unwrap()),
            );
            if p99 >= P99_LIMIT {
                failures.push(format!("{name} {kind}: p99 {:.1} ms", millis(p99)));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "reconstruction p99 must stay under {} ms (design §5.8): {}",
        P99_LIMIT.as_millis(),
        failures.join(", ")
    );
}
