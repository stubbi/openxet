//! The fetch gate (design §16 "Speed", ADR 0004 item 6): serving a clone of
//! S9's 10k-commit repository through openxet's upload-pack costs at most
//! [`MAX_RATIO`] times the CPU `git upload-pack` alone spends on the same
//! request, both measured here on this machine ([`clone_replay`]). S9
//! measured git at 0.18 s for its `tf10k`; measuring git in the same run
//! makes the bar the machine's own, so a slow runner doesn't fail it.
//!
//! [`RUNS`] alternating runs of each after a warm-up; the medians are
//! compared. It holds in release builds only, as CI runs it:
//! `cargo nextest run --release -p openxet-git -E 'binary(fetch_gate)' --no-capture`.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss)]

// The benchmark reads what the gate doesn't.
#[allow(dead_code)]
#[path = "fixture/clone_replay.rs"]
mod clone_replay;

use clone_replay::{COMMITS, median, replay};

/// openxet's CPU over git's, at most.
const MAX_RATIO: f64 = 2.0;
/// Measured runs of each.
const RUNS: usize = 7;

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    debug_assertions,
    ignore = "a CPU gate holds in release builds: run it with --release"
)]
async fn clone_costs_at_most_twice_git_cpu() {
    let replay = replay().await;
    let request = replay.clone_request();
    let warm_git = replay.git(&request);
    let warm_openxet = replay.openxet(&request).await;
    // The same pack, so the same work: only the side-band framing, which
    // follows how git's writes fall, may differ.
    assert!(
        warm_openxet.bytes.abs_diff(warm_git.bytes) * 100 < warm_git.bytes,
        "openxet sent {} bytes, git {}",
        warm_openxet.bytes,
        warm_git.bytes
    );

    let (mut git, mut openxet) = (Vec::new(), Vec::new());
    for _ in 0..RUNS {
        git.push(replay.git(&request).cpu);
        openxet.push(replay.openxet(&request).await.cpu);
    }
    let (git, openxet) = (median(&mut git), median(&mut openxet));
    let ratio = openxet.as_secs_f64() / git.as_secs_f64();
    eprintln!(
        "clone of {COMMITS} commits ({} bytes): git {:.1} ms CPU, openxet {:.1} ms CPU, ratio {ratio:.2}",
        warm_git.bytes,
        git.as_secs_f64() * 1000.0,
        openxet.as_secs_f64() * 1000.0,
    );
    assert!(
        ratio <= MAX_RATIO,
        "serving a clone must cost at most {MAX_RATIO}x git's CPU: git {:.1} ms, openxet {:.1} ms",
        git.as_secs_f64() * 1000.0,
        openxet.as_secs_f64() * 1000.0,
    );
}
