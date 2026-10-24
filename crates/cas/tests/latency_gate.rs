//! The reconstruction latency gate (design §5.8, §16 "Speed"):
//! reconstruction p99 < 50 ms for files up to 100k terms.
//!
//! The file shape is pinned, so the gate can't pass by measuring an easy
//! file. Both files have 100,000 terms over 1,000 xorbs of 1,024 chunks
//! ([`large_files`]): one with every term in its own fetch descriptor, so a
//! full answer signs 100,000 URLs (the worst case), and one merged into
//! 1,000 descriptors.
//!
//! Each shape is asked 200 full-file and 200 random-range
//! `GET /v2/reconstructions` in process, one at a time, through the whole
//! router and a SQLite index on disk. Latency runs from the request to the
//! last byte of the body. The gate holds in release builds only, as CI runs
//! it: `cargo nextest run --release -p openxet-cas -E 'binary(latency_gate)'`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "fixture/large_files.rs"]
mod large_files;

use std::time::Duration;

use large_files::{fixture, get, urls};

const TERMS: u32 = 100_000;
/// Requests of each kind per shape.
const REQUESTS: usize = 200;
/// Design §5.8.
const P99_LIMIT: Duration = Duration::from_millis(50);

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
    let fixture = fixture(TERMS).await;
    let mut rng = Rng(0x0e7a_11fe_9a7e_0001);
    let mut failures = Vec::new();
    for (shape, record) in &fixture.files {
        let name = shape.name();
        // The shape is what the gate claims: one URL per descriptor.
        let (_, body) = get(&fixture.state, &record.hash, None).await;
        assert_eq!(
            urls(&body),
            shape.descriptors(TERMS),
            "{name}: the file's shape changed"
        );

        let mut full = Vec::with_capacity(REQUESTS);
        let mut ranged = Vec::with_capacity(REQUESTS);
        for _ in 0..REQUESTS {
            full.push(get(&fixture.state, &record.hash, None).await.0);
            let start = rng.below(record.size);
            let end = start + rng.below(record.size - start);
            ranged.push(
                get(&fixture.state, &record.hash, Some((start, end)))
                    .await
                    .0,
            );
        }
        for (kind, samples) in [("full", &mut full), ("range", &mut ranged)] {
            let p99 = p99(samples);
            eprintln!(
                "{name} {kind}: p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms (full body {} B)",
                millis(samples[samples.len() / 2]),
                millis(p99),
                millis(*samples.last().unwrap()),
                body.len(),
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
