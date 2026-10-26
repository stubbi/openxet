//! Hub metadata latency, for humans: the latency gate
//! (`tests/hub_latency_gate.rs`) is what CI enforces, on token, resolve
//! (`HEAD`) and tree. This also times the reads the gate leaves out:
//! `resolve` with `GET`, a `tree` page with `expand`, a recursive `tree`
//! page and a `paths-info` of 100 paths. Results and the machine they were
//! taken on are in `docs/reference/benchmarks.md`.
//!
//! The fixture is the gate's (`tests/fixture/metadata.rs`): 10,000
//! repositories, one of them holding 10,000 files. Each request is asked
//! [`REQUESTS`] times in process, one at a time, and timed to the body's
//! last byte; prints p50, p99 and max.
//!
//! Run with `cargo bench -p openxet-hub --bench metadata`.

#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    missing_docs
)]

#[allow(dead_code)]
#[path = "../tests/fixture/metadata.rs"]
mod metadata;

use std::time::Duration;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, CONTENT_TYPE};
use axum::http::{Method, Request, StatusCode};
use metadata::{BIG, REGULAR_FILES, TOKEN, XET_FILES, fixture, regular_path, timed, xet_path};
use openxet_hub::test_support::BEARER;
use tower::ServiceExt as _;

const REQUESTS: usize = 1_000;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// xorshift64*, fixed seed: the same requests on every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_f491_4f6c_dd1d) % u64::from(n)) as u32
    }
}

fn report(name: &str, samples: &mut [Duration]) {
    samples.sort_unstable();
    let p99 = samples[(samples.len() * 99).div_ceil(100) - 1];
    println!(
        "| {name} | {:.2} | {:.2} | {:.2} |",
        ms(samples[samples.len() / 2]),
        ms(p99),
        ms(*samples.last().unwrap())
    );
}

#[allow(clippy::too_many_lines)]
fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let fx = runtime.block_on(fixture());
    let hub = &fx.hub;
    let mut rng = Rng(0x0e7a_11fe_9a7e_0003);
    println!("| Request | p50 ms | p99 ms | max ms |");
    println!("|---|---|---|---|");

    let kinds: [(&str, Method, StatusCode); 8] = [
        ("token (read)", Method::GET, StatusCode::OK),
        ("token (write)", Method::GET, StatusCode::OK),
        ("resolve HEAD, Xet file", Method::HEAD, StatusCode::FOUND),
        ("resolve HEAD, regular file", Method::HEAD, StatusCode::OK),
        ("resolve GET, regular file", Method::GET, StatusCode::OK),
        ("tree, root's first page", Method::GET, StatusCode::OK),
        ("tree, first page with expand", Method::GET, StatusCode::OK),
        ("tree, recursive first page", Method::GET, StatusCode::OK),
    ];
    for (name, method, expected) in kinds {
        let mut samples = Vec::with_capacity(REQUESTS);
        for _ in 0..REQUESTS {
            let repo = &fx.repos[rng.below(fx.repos.len() as u32) as usize];
            let path = match name {
                "token (read)" => format!("/api/models/{repo}/xet-read-token/main"),
                "token (write)" => format!("/api/models/{repo}/xet-write-token/main"),
                "resolve HEAD, Xet file" => {
                    format!("/{BIG}/resolve/main/{}", xet_path(rng.below(XET_FILES)))
                }
                "resolve HEAD, regular file" | "resolve GET, regular file" => format!(
                    "/{BIG}/resolve/main/{}",
                    regular_path(rng.below(REGULAR_FILES))
                ),
                "tree, root's first page" => format!("/api/models/{BIG}/tree/main"),
                "tree, first page with expand" => {
                    format!("/api/models/{BIG}/tree/main?expand=true")
                }
                _ => format!("/api/models/{BIG}/tree/main?recursive=true"),
            };
            let (took, _) = runtime.block_on(timed(hub, method.clone(), &path, expected));
            samples.push(took);
        }
        report(name, &mut samples);
    }

    // paths-info of 100 random paths, as `get_paths_info` batches them.
    let mut samples = Vec::with_capacity(REQUESTS);
    for _ in 0..REQUESTS {
        let form: Vec<String> = (0..100)
            .map(|_| format!("paths={}", xet_path(rng.below(XET_FILES))))
            .collect();
        let request = Request::builder()
            .method(Method::POST)
            .uri(format!("/api/models/{BIG}/paths-info/main"))
            .header(AUTHORIZATION, format!("{BEARER} {TOKEN}"))
            .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(form.join("&")))
            .unwrap();
        let started = std::time::Instant::now();
        let response = runtime
            .block_on(hub.router.clone().oneshot(request))
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        runtime
            .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
            .unwrap();
        samples.push(started.elapsed());
    }
    report("paths-info, 100 Xet files", &mut samples);
}
