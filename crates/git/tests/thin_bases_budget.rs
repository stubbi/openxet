//! Measures the thin-base fallback walk (ADR 0004 item 3) over S9's
//! 10k-commit repository, the numbers `git.thin_bases`'s defaults are set
//! from (ruling M2-R16, `docs/reference/benchmarks.md`), and checks that
//! the defaults place a base as deep as a repository of that size has.
//!
//! Each push is the fallback test's (`thin_bases.rs`) at full depth: a
//! commit on `main` adding one blob, sent as a delta against the root
//! commit's version of a file every later commit has rewritten. That base
//! is in no boundary or tip tree, so only the walk places it, and the walk
//! lists the root commit's objects last: it visits nearly every object the
//! refs reach. In release builds only, as CI runs it:
//! `cargo nextest run --release -p openxet-git -E 'binary(thin_bases_budget)' --no-capture`.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss)]

mod common;
mod push_support;

use std::time::{Duration, Instant};

use openxet_git::Budget;

use common::{World, fixture_repo};
use push_support::{blob, commit_tree, config, engine, must, oid, pack_thin, raw_push, seeded};

/// Commits, as S9's `tf10k`.
const COMMITS: u32 = 10_001;
/// Measured pushes.
const RUNS: usize = 5;
/// The file whose root-commit version each push deltas against.
const PATH: &str = "src/dir0/file0.txt";

fn millis(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[tokio::test(flavor = "multi_thread")]
#[cfg_attr(
    debug_assertions,
    ignore = "a timed budget holds in release builds: run it with --release"
)]
async fn defaults_place_the_deepest_base_of_a_10k_commit_repo() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, COMMITS).await;
    let src = fixture_repo(&fx);
    let main = oid(&src, "refs/heads/main");
    let root = oid(&src, &format!("main~{}", COMMITS - 1));
    let base = oid(&src, &format!("{root}:{PATH}"));
    assert_ne!(
        base,
        oid(&src, &format!("main:{PATH}")),
        "the base must miss the tip's tree"
    );

    // What the walk lists: every object the refs reach, and how far in it
    // finds the base.
    let started = Instant::now();
    let listed = must(
        &src,
        &["rev-list", "--objects", "refs/heads/main", "refs/tags/v1.0"],
    );
    let full_walk = started.elapsed();
    let objects = listed.lines().count() as u64;
    let visited = listed
        .lines()
        .position(|l| l.starts_with(&base.to_string()))
        .unwrap() as u64
        + 1;

    let node = engine(&world, config(&world, "node")).await;
    let contents = must(&src, &["cat-file", "blob", &base.to_string()]);
    let mut pushes = Vec::with_capacity(RUNS);
    for i in 0..RUNS {
        let mut changed = contents.clone().into_bytes();
        changed.extend_from_slice(format!("value = push {i}\n").as_bytes());
        let d = blob(&src, &changed);
        let moved = commit_tree(&src, "main^{tree}", &[main], &[("moved.txt", d)]);
        let tree = oid(&src, &format!("{moved}^{{tree}}"));
        let pack = pack_thin(&src, root, &[(moved, ""), (tree, ""), (d, PATH)]);
        let name = format!("refs/heads/moved-{i}");
        let before = node.writer_rule_counts();
        let started = Instant::now();
        let r = raw_push(
            &node,
            repo,
            "pusher",
            &[(None, Some(moved), &name)],
            Some(pack),
        )
        .await;
        pushes.push(started.elapsed());
        assert!(r.ok(&name), "push {i}: {r:?}");
        assert_eq!(
            node.writer_rule_counts().walk - before.walk,
            1,
            "push {i} took the walk"
        );
    }
    pushes.sort_unstable();
    let push = pushes[RUNS / 2];
    eprintln!(
        "{COMMITS} commits, {objects} objects: the walk placed the base after {visited}; \
         a full `rev-list --objects` took {:.0} ms, a push through the walk {:.0} ms \
         (median of {RUNS}), {:.0} objects/ms",
        millis(full_walk),
        millis(push),
        objects as f64 / millis(full_walk),
    );

    let defaults = Budget::THIN_BASES;
    assert!(
        objects <= defaults.max_objects,
        "a full walk of {objects} objects must fit git.thin_bases.max_objects ({})",
        defaults.max_objects
    );
    assert!(
        push < defaults.max_time,
        "a push through the walk took {:.0} ms, past git.thin_bases.max_ms ({} ms)",
        millis(push),
        defaults.max_time.as_millis()
    );
}
