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
//!
//! Each kind of request prints one `openxet.latency-gate.v1` JSON line on
//! stdout. `OPENXET_LATENCY_GATE` picks what the run does with them:
//! `assert` (the default) fails past 50 ms; `measure` only prints, so CI can
//! compare a pull request with its base measured on the same runner
//! (`scripts/check-latency-gate.py`, ruling M2-R17). Each line's `build` is
//! `OPENXET_LATENCY_GATE_BUILD` as it was when the test was compiled (CI sets
//! the checkout's commit), so the comparison can tell the two builds apart.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "fixture/large_files.rs"]
mod large_files;

use std::time::Duration;

use large_files::{fixture, get, urls};
use serde::Serialize;

const TERMS: u32 = 100_000;
/// Requests of each kind per shape.
const REQUESTS: usize = 200;
/// Design §5.8.
const P99_LIMIT: Duration = Duration::from_millis(50);
/// The schema of each printed line, which `scripts/check-latency-gate.py`
/// reads.
const SCHEMA: &str = "openxet.latency-gate.v1";

#[derive(Debug, PartialEq)]
enum Mode {
    /// p99 < 50 ms or the test fails.
    Assert,
    /// Prints the measurements; the comparison happens in CI (M2-R17).
    Measure,
}

impl Mode {
    fn parse(value: Option<&str>) -> Result<Self, String> {
        match value {
            None | Some("assert") => Ok(Self::Assert),
            Some("measure") => Ok(Self::Measure),
            Some(other) => Err(format!(
                "OPENXET_LATENCY_GATE={other:?} is not a mode: use `assert` (the default) or `measure`"
            )),
        }
    }
}

/// The commit this binary was compiled from, when the build says (M2-R17).
/// Read at compile time, so a test binary left over from another tree reports
/// that tree's commit.
const BUILD: Option<&str> = option_env!("OPENXET_LATENCY_GATE_BUILD");

/// One kind of request's measurements, as CI reads them.
#[derive(Serialize)]
struct Line<'a> {
    schema: &'a str,
    build: Option<&'a str>,
    shape: &'a str,
    p50_ms: f64,
    p99_ms: f64,
    max_ms: f64,
    runs: usize,
}

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

/// Microsecond precision, so the line stays short.
fn rounded_millis(d: Duration) -> f64 {
    (millis(d) * 1000.0).round() / 1000.0
}

/// The JSON line for `samples` of `shape`, and their p99.
fn json_line(shape: &str, samples: &mut [Duration]) -> (String, Duration) {
    let p99 = p99(samples);
    let line = Line {
        schema: SCHEMA,
        build: BUILD,
        shape,
        p50_ms: rounded_millis(samples[samples.len() / 2]),
        p99_ms: rounded_millis(p99),
        max_ms: rounded_millis(*samples.last().unwrap()),
        runs: samples.len(),
    };
    (serde_json::to_string(&line).unwrap(), p99)
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    debug_assertions,
    ignore = "a latency gate holds in release builds: run it with --release"
)]
async fn reconstruction_p99_under_50ms_for_100k_terms() {
    let mode = Mode::parse(std::env::var("OPENXET_LATENCY_GATE").ok().as_deref())
        .unwrap_or_else(|e| panic!("{e}"));
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
            let (line, p99) = json_line(&format!("{name} {kind}"), samples);
            println!("{line}");
            eprintln!(
                "{name} {kind}: p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms (full body {} B)",
                millis(samples[samples.len() / 2]),
                millis(p99),
                millis(*samples.last().unwrap()),
                body.len(),
            );
            if mode == Mode::Assert && p99 >= P99_LIMIT {
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

#[test]
fn json_line_carries_what_ci_reads() {
    let mut samples: Vec<Duration> = (1..=200).rev().map(Duration::from_millis).collect();
    let (line, p99) = json_line("unmerged full", &mut samples);
    assert_eq!(p99, Duration::from_millis(198));
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!({
            "schema": "openxet.latency-gate.v1",
            "build": BUILD,
            "shape": "unmerged full",
            "p50_ms": 101.0,
            "p99_ms": 198.0,
            "max_ms": 200.0,
            "runs": 200,
        })
    );
    assert!(!line.contains('\n'), "one line per shape");
}

#[test]
fn mode_defaults_to_assert_and_rejects_unknown_values() {
    assert_eq!(Mode::parse(None), Ok(Mode::Assert));
    assert_eq!(Mode::parse(Some("assert")), Ok(Mode::Assert));
    assert_eq!(Mode::parse(Some("measure")), Ok(Mode::Measure));
    let err = Mode::parse(Some("off")).unwrap_err();
    assert!(
        err.contains("`assert`") && err.contains("`measure`"),
        "{err}"
    );
}
