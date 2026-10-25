//! R22 for readers (design §5.9 requirement 8, ADR 0004 item 2): a fetch
//! gets only objects the snapshot's refs reach. Wants are checked before
//! git runs, by ref tips and tag chains first and then one budgeted commit
//! walk; everything else fails closed with one text.

mod common;

use std::future::Future as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::task::Poll;
use std::time::{Duration, Instant};

use common::fetch::{Scenario, not_our_ref, scenario, upload, v2_fetch};
use common::{World, ok};
use openxet_git::{Budget, Engine, GitConfig, GitError, Reachability, Refused};
use openxet_index::Oid;

fn has_packfile(response: &[u8]) -> bool {
    response.windows(9).any(|w| w == b"packfile\n")
}

/// The four routes to an object no ref reaches (ADR 0004 item 2): never
/// reachable, force-pushed away, only in a pending pack, only in a lost
/// pack. Each is refused with the same text, before git runs; a reachable
/// commit that is no ref's tip is served.
#[tokio::test]
async fn v2_want_unreachable_refused() {
    let world = World::new().await;
    let s = scenario(&world).await;
    let engine = world.engine().await;

    for (case, want) in [
        ("unreachable", s.dangling),
        ("force-pushed away", s.away),
        ("pending", s.pending),
        ("lost push", s.lost),
    ] {
        let response = upload(&engine, s.repo, 2, v2_fetch(&[want])).await;
        assert_eq!(
            response,
            not_our_ref(want),
            "{case}: {}",
            String::from_utf8_lossy(&response)
        );
        // Beside a reachable want, too: one bad want refuses the request.
        let response = upload(&engine, s.repo, 2, v2_fetch(&[s.main, want])).await;
        assert_eq!(response, not_our_ref(want), "{case} beside the tip");
    }

    let deep = s.at("main~12");
    let response = upload(&engine, s.repo, 2, v2_fetch(&[deep])).await;
    assert!(
        has_packfile(&response),
        "a reachable non-tip commit: {}",
        String::from_utf8_lossy(&response)
    );
}

async fn check(
    reach: &Reachability,
    engine: &Engine,
    s: &Scenario,
    wants: &[Oid],
) -> Result<(), Refused> {
    let snapshot = ok(engine.snapshot(s.repo).await);
    reach.check(&snapshot, wants, "tester").await
}

fn budget(max_objects: u64) -> Budget {
    Budget {
        max_objects,
        ..Budget::REACHABILITY
    }
}

#[tokio::test]
async fn reachability_budget_enforced() {
    let world = World::new().await;
    let s = scenario(&world).await;
    let engine = world.engine().await;
    let near = s.at("main~11");
    let far = s.at("main~20");

    // Commits: main~20 is five commits under t15's main~15 and ten under
    // main, so four visited commits can't reach it.
    let small = Reachability::new(budget(4));
    assert_eq!(check(&small, &engine, &s, &[near]).await, Ok(()));
    assert_eq!(
        check(&small, &engine, &s, &[near, far]).await,
        Err(Refused::NotOurRef(far))
    );
    let default = Reachability::new(Budget::REACHABILITY);
    assert_eq!(check(&default, &engine, &s, &[near, far]).await, Ok(()));

    // No walk at all: tips and tag chains still pass.
    let none = Reachability::new(budget(0));
    assert_eq!(
        check(&none, &engine, &s, &[s.main, s.t15, s.t15_target]).await,
        Ok(())
    );
    assert_eq!(
        check(&none, &engine, &s, &[near]).await,
        Err(Refused::NotOurRef(near))
    );

    // Time.
    let instant = Reachability::new(Budget {
        max_time: Duration::ZERO,
        ..Budget::REACHABILITY
    });
    assert_eq!(
        check(&instant, &engine, &s, &[near]).await,
        Err(Refused::NotOurRef(near))
    );
    assert_eq!(check(&instant, &engine, &s, &[s.main]).await, Ok(()));

    // Concurrency: any want off the ref tips takes both slots before a
    // lookup, so an unreachable commit, a tag target and an absent object
    // are all refused alike; tips take none.
    for (limits, refused) in [
        ((Some(0), None), Refused::RepoBusy),
        ((None, Some(0)), Refused::PrincipalBusy),
    ] {
        let busy = Reachability::new(Budget {
            max_concurrent_per_repo: limits.0,
            max_concurrent_per_principal: limits.1,
            ..Budget::REACHABILITY
        });
        for want in [near, s.dangling, s.t15_target, Oid([0x42; 20])] {
            let got = check(&busy, &engine, &s, &[want]).await;
            assert_eq!(got, Err(refused.clone()), "{want}");
        }
        assert!(refused.retryable());
        assert_eq!(check(&busy, &engine, &s, &[s.main, s.t15]).await, Ok(()));
    }

    // Trees and blobs only as a direct ref or tag target.
    assert_eq!(
        check(&default, &engine, &s, &[s.blob, s.treetag, s.tagged_tree]).await,
        Ok(())
    );
    for object in [s.other_tree, s.other_blob] {
        assert_eq!(
            check(&default, &engine, &s, &[object]).await,
            Err(Refused::NotOurRef(object))
        );
    }
}

/// Over a concurrency limit the engine refuses retryably instead of
/// answering in the stream, so smart HTTP can answer 429.
#[tokio::test]
async fn over_the_limit_is_retryable() {
    let world = World::new().await;
    let s = scenario(&world).await;
    let config = GitConfig {
        reachability: Budget {
            max_concurrent_per_repo: Some(0),
            ..Budget::REACHABILITY
        },
        ..world.config()
    };
    let engine = ok(Engine::start(world.index.clone(), world.bucket.clone(), config).await);
    let near = s.at("main~11");
    let refused = engine
        .upload_pack(s.repo, 2, "tester", common::fetch::body(v2_fetch(&[near])))
        .await;
    assert!(
        matches!(&refused, Err(GitError::Refused(Refused::RepoBusy))),
        "{:?}",
        refused.as_ref().err()
    );
    let served = upload(&engine, s.repo, 2, v2_fetch(&[s.main])).await;
    assert!(has_packfile(&served));
}

/// A long linear history, all of it pushed with `refs/heads/main` at the
/// tip, plus a commit on the root that no ref reaches: a want of it walks
/// the whole history.
struct Long {
    _fixture: tempfile::TempDir,
    repo: openxet_index::RepoId,
    root: Oid,
    dangling: Oid,
}

const LONG_COMMITS: u32 = 30_000;

async fn long_history(world: &World) -> Long {
    let fx = common::fixture("tf10k", LONG_COMMITS);
    let src = common::fixture_repo(&fx);
    let root_spec = format!("main~{}", LONG_COMMITS - 1);
    let root = common::rev(&src, &root_spec);
    let root_tree = format!("{root_spec}^{{tree}}");
    let dangling: Oid = ok(common::git(
        &src,
        &[
            "-c",
            "user.name=Tester",
            "-c",
            "user.email=tester@openxet.invalid",
            "commit-tree",
            &root_tree,
            "-p",
            &root_spec,
            "-m",
            "dangling",
        ],
    )
    .trim()
    .parse());
    let repo = world.repo("org", "long").await;
    let packs = world.dir.path().join("long-packs");
    let all = common::pack_objects(&src, &format!("main\n{dangling}"), &packs);
    let tip = common::rev(&src, "main");
    world
        .commit(
            repo,
            &all,
            1_761_400_000,
            &[common::set("refs/heads/main", None, tip)],
        )
        .await;
    Long {
        repo,
        root,
        dangling,
        _fixture: fx,
    }
}

/// Two walks really in flight hold the repository's two slots: a third
/// request is refused as busy, and admitted once they finish. Each walk is
/// polled once by hand, which takes its slots, before it is spawned, so
/// the probe can't take a slot first. The probe wants an absent object,
/// which costs no walk when admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_walks_hold_the_slots() {
    let world = World::new().await;
    let long = long_history(&world).await;
    let engine = world.engine().await;
    let reach = std::sync::Arc::new(Reachability::new(Budget {
        max_objects: u64::MAX,
        max_time: Duration::from_mins(2),
        max_concurrent_per_repo: Some(2),
        max_concurrent_per_principal: None,
    }));
    let snapshot = std::sync::Arc::new(ok(engine.snapshot(long.repo).await));
    let mut walks = Vec::new();
    for who in ["a", "b"] {
        let (reach, snapshot, want) = (reach.clone(), snapshot.clone(), long.dangling);
        let mut walk = Box::pin(async move { reach.check(&snapshot, &[want], who).await });
        let first = std::future::poll_fn(|cx| Poll::Ready(walk.as_mut().poll(cx))).await;
        assert!(first.is_pending(), "{who}'s walk ended at once: {first:?}");
        walks.push(tokio::spawn(walk));
    }
    let absent = Oid([0x42; 20]);
    assert_eq!(
        reach.check(&snapshot, &[absent], "probe").await,
        Err(Refused::RepoBusy),
        "a third request while two walks run"
    );
    assert!(
        walks.iter().all(|w| !w.is_finished()),
        "the walks must still have been running"
    );
    for walk in walks {
        assert_eq!(ok(walk.await), Err(Refused::NotOurRef(long.dangling)));
    }
    assert_eq!(
        reach.check(&snapshot, &[absent], "probe").await,
        Err(Refused::NotOurRef(absent)),
        "admitted once the walks gave their slots back"
    );
}

/// The walk looks at the clock every 256 commits. The check reads a clock
/// the test drives, one millisecond further on every read, so where the
/// 20 ms budget runs out depends only on how many times it was read: past
/// the reads before the walk (the start, one per ref, the walk's start),
/// within the walk to the root of the 30,000-commit history, so the
/// in-loop check stops it. The same budget with a clock that never moves
/// reaches the root, so nothing but that check refused it.
#[tokio::test]
async fn time_budget_stops_a_running_walk() {
    let world = World::new().await;
    let long = long_history(&world).await;
    let engine = world.engine().await;
    let snapshot = ok(engine.snapshot(long.repo).await);
    let budget = Budget {
        max_objects: u64::MAX,
        max_time: Duration::from_millis(20),
        ..Budget::REACHABILITY
    };
    let base = Instant::now();
    let reads = Arc::new(AtomicU32::new(0));
    let ticking = {
        let reads = Arc::clone(&reads);
        Arc::new(move || {
            base + Duration::from_millis(u64::from(reads.fetch_add(1, Ordering::SeqCst)))
        })
    };
    let timed = Reachability::with_clock(budget, ticking);
    assert_eq!(
        timed.check(&snapshot, &[long.root], "tester").await,
        Err(Refused::NotOurRef(long.root))
    );
    let reads = reads.load(Ordering::SeqCst);
    let before_walk = 3;
    let whole_walk = LONG_COMMITS / 256;
    assert!(
        reads > before_walk && reads < before_walk + whole_walk,
        "{reads} clock reads: the walk must have been stopped inside its loop"
    );
    let frozen = Reachability::with_clock(budget, Arc::new(move || base));
    assert_eq!(
        frozen.check(&snapshot, &[long.root], "tester").await,
        Ok(())
    );
}
