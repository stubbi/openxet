//! Durable commit throughput on local storage, for humans. Results and the
//! machine they were taken on are in `docs/reference/benchmarks.md`.
//!
//! - **Shard commits** (ruling M1-R24): 400 shards of 4 files each, every
//!   one a 64 KiB `LocalBucket` put of the stored shard followed by its
//!   index registration, committed concurrently under a semaphore of 1 to
//!   128 permits, as the `commit` semaphore bounds them.
//! - **Index commit pairs**: 200 pairs of `put_xorb` and `register_shard`
//!   (4 files), each its own durable SQLite transaction, one after another
//!   and then concurrently: the cost of one durable index commit.
//!
//! Each run starts on a fresh bucket and index in the system temporary
//! directory, so it measures that disk. Run with
//! `cargo bench -p openxet-cas --bench commit_throughput`.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, missing_docs)]

use std::sync::Arc;
use std::time::Instant;

use axum::body::Bytes;
use openxet_index::{
    ChunkTable, FileRecord, Index, NewFile, Registrant, SqliteIndex, Term, Visibility,
};
use openxet_protocol::{MerkleHash, chunk_hash};
use openxet_storage::{Bucket, Key, LocalBucket, Registration};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const SCOPE: &str = "bench";
/// The tenant the bench registers for.
const TENANT: &str = "scope:bench";

/// A registration to [`SCOPE`] numbered `seq`.
fn by(seq: u64) -> Registrant<'static> {
    Registrant {
        scope: SCOPE,
        tenant: TENANT,
        seq,
    }
}
const SHARDS: usize = 400;
const FILES_PER_SHARD: u32 = 4;
const STORED_SHARD_BYTES: usize = 64 * 1024;
const PAIRS: usize = 200;

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

fn table(label: &str) -> ChunkTable {
    let hashes: Vec<(MerkleHash, u64)> = (0..FILES_PER_SHARD)
        .map(|i| (h(&format!("{label}/c{i}")), 1000))
        .collect();
    ChunkTable {
        hashes: hashes.into(),
        offsets: (0..=FILES_PER_SHARD)
            .map(|i| i * 600)
            .collect::<Vec<_>>()
            .into(),
    }
}

/// One file per chunk of the xorb, one term each.
fn files(label: &str, xorb: MerkleHash) -> Vec<NewFile> {
    (0..FILES_PER_SHARD)
        .map(|f| NewFile {
            record: FileRecord {
                hash: h(&format!("{label}/f{f}")),
                size: 1000,
                terms: vec![Term {
                    xorb,
                    chunk_start: f,
                    chunk_end: f + 1,
                    unpacked_bytes: 1000,
                }],
            },
            sha256: None,
            first_chunk: Some(h(&format!("{label}/c{f}"))),
        })
        .collect()
}

async fn put_xorb(index: &SqliteIndex, label: &str) -> MerkleHash {
    let xorb = h(&format!("{label}/x"));
    index
        .put_xorb(&xorb, 2400, &table(label), &[], TENANT)
        .await
        .unwrap();
    xorb
}

async fn shard_commits(permits: usize) -> f64 {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(LocalBucket::open(&dir.path().join("bucket")).unwrap());
    let index = Arc::new(
        SqliteIndex::open(&dir.path().join("index.sqlite"))
            .await
            .unwrap(),
    );
    let mut xorbs = Vec::with_capacity(SHARDS);
    for i in 0..SHARDS {
        xorbs.push(put_xorb(&index, &format!("s{i}")).await);
    }
    let commit = Arc::new(Semaphore::new(permits));
    let started = Instant::now();
    let mut set = JoinSet::new();
    for (i, xorb) in xorbs.into_iter().enumerate() {
        let (bucket, index, commit) = (bucket.clone(), index.clone(), commit.clone());
        set.spawn(async move {
            let label = format!("s{i}");
            let shard = h(&format!("{label}/shard"));
            let body = Bytes::from(vec![u8::try_from(i % 251).unwrap(); STORED_SHARD_BYTES]);
            let _permit = commit.acquire().await.unwrap();
            bucket.put(&Key::Shard(shard), body).await.unwrap();
            // As `ingest` does: the registration record, then the index.
            let seq = index.next_registration_seq().await.unwrap();
            let record = Registration {
                scope: SCOPE.to_owned(),
                tenant: TENANT.to_owned(),
                shard,
                seq,
            };
            bucket
                .create(&record.key(), record.to_bytes())
                .await
                .unwrap();
            index
                .register_shard(&shard, &files(&label, xorb), by(seq), Visibility::Global)
                .await
                .unwrap();
        });
    }
    while set.join_next().await.is_some() {}
    SHARDS as f64 / started.elapsed().as_secs_f64()
}

async fn commit_pair(index: Arc<SqliteIndex>, i: usize) {
    let label = format!("p{i}");
    let xorb = put_xorb(&index, &label).await;
    let seq = index.next_registration_seq().await.unwrap();
    index
        .register_shard(
            &h(&format!("{label}/shard")),
            &files(&label, xorb),
            by(seq),
            Visibility::Global,
        )
        .await
        .unwrap();
}

async fn index_commit_pairs() -> (f64, f64) {
    let dir = tempfile::tempdir().unwrap();
    let index = Arc::new(
        SqliteIndex::open(&dir.path().join("index.sqlite"))
            .await
            .unwrap(),
    );
    let started = Instant::now();
    for i in 0..PAIRS {
        commit_pair(index.clone(), i).await;
    }
    let sequential = PAIRS as f64 / started.elapsed().as_secs_f64();
    let started = Instant::now();
    let mut set = JoinSet::new();
    for i in PAIRS..2 * PAIRS {
        set.spawn(commit_pair(index.clone(), i));
    }
    while set.join_next().await.is_some() {}
    (sequential, PAIRS as f64 / started.elapsed().as_secs_f64())
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        println!("shard commits ({SHARDS} shards, {FILES_PER_SHARD} files, 64 KiB each)");
        for permits in [1, 2, 4, 8, 16, 32, 64, 128] {
            println!("  {permits:>3} permits: {:>5.0} shards/s", shard_commits(permits).await);
        }
        println!("index commit pairs ({PAIRS} each way)");
        for round in 1..=3 {
            let (sequential, concurrent) = index_commit_pairs().await;
            println!(
                "  round {round}: sequential {:.2} ms per pair ({sequential:.0} pairs/s), concurrent {concurrent:.0} pairs/s",
                1000.0 / sequential
            );
        }
    });
}
