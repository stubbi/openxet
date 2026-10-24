//! Seeds a data directory for `scripts/measure-memory-bounds.py`, which
//! measures a served process's peak RSS under the memory attacks of ruling
//! M1-R40 (`docs/reference/benchmarks.md`, "Memory bounds").
//!
//! Registers the latency gate's two 100k-term files ([`large_files`]) to
//! the dev scope in `$OPENXET_SEED_DIR/index.sqlite`, and writes their
//! hashes to `$OPENXET_SEED_DIR/seeded.json`. Ignored unless that variable
//! is set, since it only prepares a measurement.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "fixture/large_files.rs"]
#[allow(dead_code, reason = "the seed uses only part of the shared fixture")]
mod large_files;

use std::path::PathBuf;

use openxet_index::SqliteIndex;
use openxet_protocol::hash_to_hex;

/// The scope `openxet serve` gives its dev token.
const DEV_SCOPE: &str = "default";
const TERMS: u32 = 100_000;

#[tokio::test(flavor = "multi_thread")]
async fn seed_large_files() {
    let Some(dir) = std::env::var_os("OPENXET_SEED_DIR").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let index = SqliteIndex::open(&dir.join("index.sqlite")).await.unwrap();
    let files = large_files::seed(&index, DEV_SCOPE, TERMS).await;
    let seeded: serde_json::Map<String, serde_json::Value> = files
        .iter()
        .map(|(shape, record)| (shape.name().to_owned(), hash_to_hex(&record.hash).into()))
        .collect();
    std::fs::write(
        dir.join("seeded.json"),
        serde_json::to_vec(&seeded).unwrap(),
    )
    .unwrap();
}
