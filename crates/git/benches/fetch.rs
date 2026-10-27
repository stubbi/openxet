//! Spike S9's server-only fetch replay, for humans: the fetch gate
//! (`tests/fetch_gate.rs`) is what CI enforces. Results and the machine
//! they were taken on are in `docs/reference/benchmarks.md`.
//!
//! For a clone of the 10k-commit repository and a fetch 100 commits behind
//! `main`, prints the median CPU, wall time and response bytes of openxet's
//! upload-pack and of `git upload-pack` alone, over [`RUNS`] alternating
//! runs after a warm-up (`tests/fixture/clone_replay.rs`).
//!
//! Run with `cargo bench -p openxet-git --bench fetch`.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, missing_docs)]

#[path = "../tests/fixture/clone_replay.rs"]
mod clone_replay;

use std::time::Duration;

use clone_replay::{COMMITS, Served, median, replay};

const RUNS: usize = 9;

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn row(request: &str, engine: &str, served: &mut [Served]) {
    let mut cpu: Vec<Duration> = served.iter().map(|s| s.cpu).collect();
    let mut wall: Vec<Duration> = served.iter().map(|s| s.wall).collect();
    println!(
        "| {request} | {engine} | {:.1} | {:.1} | {} |",
        ms(median(&mut cpu)),
        ms(median(&mut wall)),
        served[0].bytes,
    );
}

fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let replay = runtime.block_on(replay());
    println!("tf10k fixture, {COMMITS} commits, medians of {RUNS} runs\n");
    println!("| Request | Engine | Server CPU ms | Server wall ms | Bytes sent |");
    println!("|---|---|---|---|---|");
    for (name, request) in [
        ("Full clone", replay.clone_request()),
        ("Fetch 100 commits (thin)", replay.fetch_behind_request(100)),
    ] {
        replay.git(&request);
        runtime.block_on(replay.openxet(&request));
        let (mut git, mut openxet) = (Vec::new(), Vec::new());
        for _ in 0..RUNS {
            git.push(replay.git(&request));
            openxet.push(runtime.block_on(replay.openxet(&request)));
        }
        row(name, "git", &mut git);
        row(name, "OpenXet", &mut openxet);
    }
}
