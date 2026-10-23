//! Several writers on one index file, as several stateless nodes would be:
//! identical writes stay idempotent and distinct writes never fail on the
//! lock.

use std::sync::Arc;

use openxet_index::{ChunkTable, FileRecord, Index, NewFile, SqliteIndex, Term, Visibility};
use openxet_protocol::{MerkleHash, chunk_hash};
use tempfile::TempDir;
use tokio::task::JoinSet;

const WRITERS: usize = 8;
const ROUNDS: usize = 20;
const SCOPE: &str = "model/org/repo";

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

fn table(label: &str, n: u32) -> ChunkTable {
    let hashes: Vec<(MerkleHash, u64)> = (0..n)
        .map(|i| (h(&format!("{label}/chunk{i}")), 1000))
        .collect();
    ChunkTable {
        hashes: hashes.into(),
        offsets: (0..=n).map(|i| i * 600).collect::<Vec<_>>().into(),
    }
}

fn files(label: &str, xorb: MerkleHash, n: u32) -> Vec<NewFile> {
    (0..2)
        .map(|f| NewFile {
            record: FileRecord {
                hash: h(&format!("{label}/file{f}")),
                size: u64::from(n) * 1000,
                terms: vec![Term {
                    xorb,
                    chunk_start: 0,
                    chunk_end: n,
                    unpacked_bytes: n * 1000,
                }],
            },
            sha256: None,
            first_chunk: Some(h(&format!("{label}/chunk0"))),
        })
        .collect()
}

/// `e` with its whole `source()` chain, so a failure names the SQLite cause
/// and not only the index's advice.
fn chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cause = c.source();
    }
    out
}

async fn open(path: &std::path::Path) -> Arc<dyn Index> {
    match SqliteIndex::open(path).await {
        Ok(index) => Arc::new(index),
        Err(e) => panic!("open index: {}", chain(&e)),
    }
}

/// Runs `WRITERS` tasks and returns each one's result.
async fn all<F>(mut task: impl FnMut(usize) -> F) -> Vec<bool>
where
    F: Future<Output = bool> + Send + 'static,
{
    let mut set = JoinSet::new();
    for i in 0..WRITERS {
        set.spawn(task(i));
    }
    let mut out = Vec::with_capacity(WRITERS);
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(v) => out.push(v),
            Err(e) => panic!("writer task failed: {e}"),
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn identical_concurrent_writes_from_separate_instances_insert_once() {
    let dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let path = dir.path().join("index.sqlite");
    let mut instances = Vec::with_capacity(WRITERS);
    for _ in 0..WRITERS {
        instances.push(open(&path).await);
    }
    for round in 0..ROUNDS {
        let label = format!("round{round}");
        let xorb = h(&format!("{label}/xorb"));
        let t = table(&label, 50);
        let eligible: Vec<MerkleHash> = t.hashes.iter().map(|(c, _)| *c).collect();

        let puts = all(|i| {
            let (index, t, eligible) = (instances[i].clone(), t.clone(), eligible.clone());
            async move {
                match index.put_xorb(&xorb, 30_100, &t, &eligible, SCOPE).await {
                    Ok(new) => new,
                    Err(e) => panic!("put_xorb: {}", chain(&e)),
                }
            }
        })
        .await;
        assert_eq!(puts.iter().filter(|new| **new).count(), 1, "round {round}");

        let fs = files(&label, xorb, 50);
        let shard = h(&format!("{label}/shard"));
        let registers = all(|i| {
            let (index, fs) = (instances[i].clone(), fs.clone());
            async move {
                match index
                    .register_shard(&shard, &fs, SCOPE, Visibility::PerScope)
                    .await
                {
                    Ok(new) => new,
                    Err(e) => panic!("register_shard: {}", chain(&e)),
                }
            }
        })
        .await;
        assert_eq!(
            registers.iter().filter(|new| **new).count(),
            1,
            "round {round}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn distinct_concurrent_writers_never_fail_on_the_lock() {
    let dir = TempDir::new().unwrap_or_else(|e| panic!("tempdir: {e}"));
    let index = open(&dir.path().join("index.sqlite")).await;
    let done = all(|w| {
        let index = index.clone();
        async move {
            for n in 0..50 {
                let label = format!("writer{w}/{n}");
                let xorb = h(&format!("{label}/xorb"));
                let t = table(&label, 4);
                if let Err(e) = index.put_xorb(&xorb, 2500, &t, &[], SCOPE).await {
                    panic!("put_xorb: {}", chain(&e));
                }
                let shard = h(&format!("{label}/shard"));
                if let Err(e) = index
                    .register_shard(&shard, &files(&label, xorb, 4), SCOPE, Visibility::PerScope)
                    .await
                {
                    panic!("register_shard: {}", chain(&e));
                }
            }
            true
        }
    })
    .await;
    assert_eq!(done.len(), WRITERS);
}
