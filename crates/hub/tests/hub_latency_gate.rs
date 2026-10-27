//! The Hub metadata latency gate (design §5.8, §16 "Speed"): token, resolve
//! (`HEAD`) and tree p99 < 10 ms, against an index of 10,000 repositories
//! and a repository of 10,000 files ([`metadata`]'s pinned shape).
//!
//! Each kind of request is asked [`REQUESTS`] times in process, one at a
//! time, through the whole router, the Git engine and a SQLite index on
//! disk, timed from the request to the body's last byte:
//! - `token`: `GET /api/models/{repo}/xet-read-token/main` for a repository
//!   drawn at random from the 10,000
//! - `resolve head xet`: `HEAD /{repo}/resolve/main/{path}` of a random Xet
//!   file, a `302` to the file gateway
//! - `resolve head regular`: the same for a random regular file, a `200`
//!   from its Git blob
//! - `tree`: `GET /api/models/{repo}/tree/main`, the root's first page of
//!   1,000 entries, 998 of them Xet files
//!
//! The gate holds in release builds only, as CI runs it:
//! `cargo nextest run --release -p openxet-hub -E 'binary(hub_latency_gate)' --no-capture`.
//! It prints one `openxet.latency-gate.v1` JSON line per kind, and reads
//! `OPENXET_LATENCY_GATE` (`assert`, the default, or `measure`) and
//! `OPENXET_LATENCY_GATE_BUILD` as the reconstruction gate does, so CI
//! compares a pull request with its base the same way (ruling M2-R17,
//! `scripts/check-latency-gate.py --p99-limit-ms 10`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "fixture/metadata.rs"]
mod metadata;

use std::time::Duration;

use axum::http::{Method, StatusCode};
use metadata::{
    BIG, REGULAR_FILES, REPOS, XET_FILES, entries, fixture, regular_path, timed, xet_path,
};
use serde::Serialize;

/// Requests of each kind.
const REQUESTS: usize = 1_000;
/// Design §5.8.
const P99_LIMIT: Duration = Duration::from_millis(10);
/// The schema of each printed line, which `scripts/check-latency-gate.py`
/// reads.
const SCHEMA: &str = "openxet.latency-gate.v1";
/// The commit this binary was compiled from, when the build says (M2-R17).
const BUILD: Option<&str> = option_env!("OPENXET_LATENCY_GATE_BUILD");

#[derive(Debug, PartialEq)]
enum Mode {
    /// p99 < 10 ms or the test fails.
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

/// xorshift64*, fixed seed: the same requests on every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let below = self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % u64::from(n);
        u32::try_from(below).expect("below n")
    }
}

fn millis(d: Duration) -> f64 {
    (d.as_secs_f64() * 1_000_000.0).round() / 1000.0
}

/// The JSON line for `samples` of `shape`, and their p99: the smallest
/// sample at least 99% are at or under.
fn json_line(shape: &str, samples: &mut [Duration]) -> (String, Duration) {
    samples.sort_unstable();
    let p99 = samples[(samples.len() * 99).div_ceil(100) - 1];
    let line = Line {
        schema: SCHEMA,
        build: BUILD,
        shape,
        p50_ms: millis(samples[samples.len() / 2]),
        p99_ms: millis(p99),
        max_ms: millis(*samples.last().unwrap()),
        runs: samples.len(),
    };
    (serde_json::to_string(&line).unwrap(), p99)
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    debug_assertions,
    ignore = "a latency gate holds in release builds: run it with --release"
)]
async fn token_resolve_and_tree_p99_under_10ms() {
    let mode = Mode::parse(std::env::var("OPENXET_LATENCY_GATE").ok().as_deref())
        .unwrap_or_else(|e| panic!("{e}"));
    let fx = fixture().await;
    let hub = &fx.hub;

    // The shape is what the gate claims.
    let (_, page) = timed(
        hub,
        Method::GET,
        &format!("/api/models/{BIG}/tree/main"),
        StatusCode::OK,
    )
    .await;
    let page = entries(&page);
    assert_eq!(page.len(), 1_000, "the root's first page");
    assert_eq!(
        page.iter().filter(|e| e.get("xetHash").is_some()).count(),
        998,
        "the first page: `.gitattributes`, `configs` and 998 Xet files"
    );

    let mut rng = Rng(0x0e7a_11fe_9a7e_0002);
    let kinds: [(&str, StatusCode); 4] = [
        ("token", StatusCode::OK),
        ("resolve head xet", StatusCode::FOUND),
        ("resolve head regular", StatusCode::OK),
        ("tree", StatusCode::OK),
    ];
    let mut failures = Vec::new();
    for (shape, expected) in kinds {
        let mut samples = Vec::with_capacity(REQUESTS);
        for _ in 0..REQUESTS {
            let (method, path) = match shape {
                "token" => {
                    let repo = &fx.repos[rng.below(REPOS) as usize];
                    (
                        Method::GET,
                        format!("/api/models/{repo}/xet-read-token/main"),
                    )
                }
                "resolve head xet" => {
                    let i = rng.below(XET_FILES);
                    (Method::HEAD, format!("/{BIG}/resolve/main/{}", xet_path(i)))
                }
                "resolve head regular" => {
                    let i = rng.below(REGULAR_FILES);
                    (
                        Method::HEAD,
                        format!("/{BIG}/resolve/main/{}", regular_path(i)),
                    )
                }
                _ => (Method::GET, format!("/api/models/{BIG}/tree/main")),
            };
            samples.push(timed(hub, method, &path, expected).await.0);
        }
        let (line, p99) = json_line(shape, &mut samples);
        println!("{line}");
        eprintln!(
            "{shape}: p50 {:.2} ms, p99 {:.2} ms, max {:.2} ms",
            millis(samples[samples.len() / 2]),
            millis(p99),
            millis(*samples.last().unwrap()),
        );
        if mode == Mode::Assert && p99 >= P99_LIMIT {
            failures.push(format!("{shape}: p99 {:.2} ms", millis(p99)));
        }
    }
    assert!(
        failures.is_empty(),
        "token, resolve and tree p99 must stay under {} ms (design §5.8): {}",
        P99_LIMIT.as_millis(),
        failures.join(", ")
    );
}

#[test]
fn json_line_carries_what_ci_reads() {
    let mut samples: Vec<Duration> = (1..=1000).rev().map(Duration::from_micros).collect();
    let (line, p99) = json_line("tree", &mut samples);
    assert_eq!(p99, Duration::from_micros(990));
    let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        parsed,
        serde_json::json!({
            "schema": "openxet.latency-gate.v1",
            "build": BUILD,
            "shape": "tree",
            "p50_ms": 0.501,
            "p99_ms": 0.99,
            "max_ms": 1.0,
            "runs": 1000,
        })
    );
}

#[test]
fn mode_defaults_to_assert_and_rejects_unknown_values() {
    assert_eq!(Mode::parse(None), Ok(Mode::Assert));
    assert_eq!(Mode::parse(Some("measure")), Ok(Mode::Measure));
    assert!(Mode::parse(Some("off")).is_err());
}
