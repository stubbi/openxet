//! R22 for readers (design §5.9 requirement 8, ADR 0004 item 2): a fetch
//! gets only objects the snapshot's refs reach. Wants are checked before
//! git runs, by ref tips and tag chains first and then one budgeted commit
//! walk; everything else fails closed with one text.

mod common;

use std::time::Duration;

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

    // Concurrency: a request needing the walk takes a slot; tips don't.
    for (limits, refused) in [
        ((Some(0), None), Refused::RepoBusy),
        ((None, Some(0)), Refused::PrincipalBusy),
    ] {
        let busy = Reachability::new(Budget {
            max_concurrent_per_repo: limits.0,
            max_concurrent_per_principal: limits.1,
            ..Budget::REACHABILITY
        });
        let got = check(&busy, &engine, &s, &[near]).await;
        assert_eq!(got, Err(refused.clone()));
        assert!(refused.retryable());
        assert_eq!(
            check(&busy, &engine, &s, &[s.main, s.t15_target]).await,
            Ok(())
        );
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
