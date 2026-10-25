//! `reindex` from the bucket after a database loss, the unplaced data it
//! persists, and the operator's restore and discard (ADR 0004 item 4,
//! design §5.3, ruling R24).

mod common;
mod lifecycle_support;
mod push_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use openxet_git::{
    DEFAULT_GRACE, DISCARD_COMMITTED_WARNING, GitError, derive_bucket_ref_log, git_command,
    reindex_discard, reindex_repo, reindex_restore, repack_full, repack_geometric, sweep_packs,
};
use openxet_index::{GitStore as _, Oid, PackState, Repo, Repos as _, SqliteIndex, UnplacedKind};
use openxet_storage::{Bucket as _, Key, PackExt, PackName, RepoIdHex};
use tempfile::TempDir;

use common::{World, ok};
use lifecycle_support::{DAY, Env, assert_i1, env, has_object, r22_refuses};
use push_support::{blob, config, engine, pack_of, raw_push};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A repository whose bucket ref log lags its last pushes, rebuilt from
/// the bucket into a new, empty database:
/// - `main` was pushed to `c1`, and the bucket ref log derived;
/// - then, not derived: `main` pushed on to `c2` (pack `p2`), a branch
///   `other` to `c3` (pack `p3`), a branch `side` to `c4` in a pack that
///   also holds `c1`, and an annotated tag `treetag` (object `tag`) of a
///   tree no commit has, in a pack that also holds `c1`.
struct Lagging {
    env: Env,
    old_db: TempDir,
    c1: Oid,
    c2: Oid,
    c3: Oid,
    c4: Oid,
    tag: Oid,
    tagged_tree: Oid,
    p2: String,
    p3: String,
}

#[allow(clippy::too_many_lines)]
async fn lagging() -> Lagging {
    let env = env(12).await;
    let base = env.oid("refs/remotes/origin/main");
    let c1 = env.new_commit(base, "c1.txt", b"c1\n");
    assert!(env.push("refs/heads/main", c1).await.ok("refs/heads/main"));
    ok(derive_bucket_ref_log(&env.node, env.repo).await);

    let c2 = env.new_commit(c1, "c2.txt", b"c2\n");
    assert!(env.push("refs/heads/main", c2).await.ok("refs/heads/main"));
    let c3 = env.new_commit(c1, "c3.txt", b"c3\n");
    assert!(
        env.push("refs/heads/other", c3)
            .await
            .ok("refs/heads/other")
    );
    let w = env.work();
    let c4 = env.new_commit(c1, "c4.txt", b"c4\n");
    let c4_objects = [
        c4,
        env.oid(&format!("{c4}^{{tree}}")),
        env.oid(&format!("{c4}:c4.txt")),
        c1,
    ];
    let r = raw_push(
        &env.node,
        env.repo,
        "pusher",
        &[(None, Some(c4), "refs/heads/side")],
        Some(pack_of(&w, &c4_objects)),
    )
    .await;
    assert!(r.ok("refs/heads/side"), "{r:?}");
    let only = blob(&w, b"only the tag keeps this\n");
    let tagged_tree: Oid =
        ok(
            common::git_with_input(&w, &["mktree"], &format!("100644 blob {only}\tonly.txt\n"))
                .trim()
                .parse(),
        );
    let tag_text = format!(
        "object {tagged_tree}\ntype tree\ntag treetag\ntagger T <t@openxet.invalid> 1761400000 +0000\n\na tree\n"
    );
    let tag: Oid = ok(common::git_with_input(&w, &["mktag"], &tag_text)
        .trim()
        .parse());
    let r = raw_push(
        &env.node,
        env.repo,
        "pusher",
        &[(None, Some(tag), "refs/tags/treetag")],
        Some(pack_of(&w, &[tag, tagged_tree, only, c1])),
    )
    .await;
    assert!(r.ok("refs/tags/treetag"), "{r:?}");

    let names_of = |oid: Oid, rows: &[openxet_index::PackRecord]| -> String {
        rows.iter()
            .find(|r| {
                let idx = env
                    .world
                    .dir
                    .path()
                    .join("node-a/repos")
                    .join(env.repo.to_string())
                    .join(format!("{}.idx", r.pack.name));
                std::fs::read(idx).is_ok_and(|b| b.windows(20).any(|w| w == oid.0))
            })
            .map_or_else(|| panic!("a pack holds {oid}"), |r| r.pack.name.clone())
    };
    drop(ok(env.node.snapshot(env.repo).await));
    let rows = env.rows().await;
    let p2 = names_of(c2, &rows);
    let p3 = names_of(c3, &rows);

    // The database is lost: a new one with the repository's row, as a
    // rebuild from its metadata record would make it, and a new node.
    let Env {
        world,
        repo,
        fx,
        client,
        ..
    } = env;
    let World {
        dir: old_db,
        bucket,
        ..
    } = world;
    let dir = ok(TempDir::new());
    let index = Arc::new(ok(SqliteIndex::open(&dir.path().join("index.sqlite")).await));
    ok(index
        .create_repo(&Repo {
            id: repo,
            namespace: "org".to_owned(),
            name: "rebuilt".to_owned(),
            private: false,
            labels: vec!["model".to_owned()],
            created_at: 1_761_400_000,
            full_repack_gen: 0,
        })
        .await);
    let world = World { dir, index, bucket };
    let node = engine(&world, config(&world, "node-r")).await;
    Lagging {
        env: Env {
            world,
            repo,
            node,
            fx,
            client,
        },
        old_db,
        c1,
        c2,
        c3,
        c4,
        tag,
        tagged_tree,
        p2,
        p3,
    }
}

/// Whether `oid`'s whole closure is in a fresh snapshot of the repository.
async fn closure_present(env: &Env, oid: Oid) -> bool {
    let snap = ok(env.node.snapshot(env.repo).await);
    ok(git_command(env.node.git(), &snap, None)
        .args(["rev-list", "--objects", "--quiet", &oid.to_string()])
        .output()
        .await)
    .status
    .success()
}

/// `reindex_unplaced_persisted` (ADR 0004 item 4, ruling R24).
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn reindex_unplaced_persisted() {
    let l = lagging().await;
    let env = &l.env;
    let started = now();
    let report = ok(reindex_repo(&env.node, env.repo, &env.lease("node-r").await).await);

    // The rebuilt refs: as far as the bucket ref log reaches, complete.
    let refs: BTreeSet<(String, Oid)> = report.refs.iter().cloned().collect();
    assert!(
        refs.contains(&("refs/heads/main".to_owned(), l.c1)),
        "{refs:?}"
    );
    assert!(
        !refs
            .iter()
            .any(|(n, _)| n == "refs/heads/other" || n == "refs/tags/treetag")
    );
    assert!(report.rolled_back.is_empty(), "{report:?}");
    assert_eq!(env.refs().await.into_iter().collect::<BTreeSet<_>>(), refs);
    assert_i1(&env.node, env.repo).await;

    // Unreached packs are recovered, timed by the bucket.
    let recovered: BTreeSet<&str> = report.recovered.iter().map(String::as_str).collect();
    assert_eq!(recovered, BTreeSet::from([l.p2.as_str(), l.p3.as_str()]));
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    for row in env.rows().await {
        if row.pack.state == PackState::Recovered {
            let key = Key::Pack {
                repo: hex,
                name: ok(PackName::parse(&row.pack.name)),
                ext: PackExt::Pack,
            };
            assert_eq!(
                Some(row.pack.created_at),
                ok(env.world.bucket.last_modified(key).await)
            );
        }
    }

    // Commit, tag, pack and edge entries, persisted.
    let kinds: Vec<UnplacedKind> = report.unplaced.iter().map(|u| u.kind).collect();
    for k in [
        UnplacedKind::Commit(l.c2),
        UnplacedKind::Commit(l.c3),
        UnplacedKind::Commit(l.c4),
        UnplacedKind::Tag(l.tag),
        UnplacedKind::Edge(l.c1),
    ] {
        assert!(kinds.contains(&k), "{k:?} in {kinds:?}");
    }
    assert_eq!(
        kinds.iter().filter(|k| **k == UnplacedKind::Pack).count(),
        2
    );
    assert!(report.unplaced.iter().all(|u| u.found_at >= started));
    let reopened = ok(SqliteIndex::open(&env.world.dir.path().join("index.sqlite")).await);
    let persisted = ok(reopened.unplaced(env.repo).await);
    let set =
        |v: &[openxet_index::Unplaced]| -> std::collections::HashSet<openxet_index::Unplaced> {
            v.iter().copied().collect()
        };
    assert_eq!(set(&persisted), set(&report.unplaced));
    drop(reopened);

    // Far past the grace period, nothing is discarded and GC keeps them.
    let far = now() + 100 * DAY;
    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.lease("node-r").await,
        DEFAULT_GRACE,
        far,
        false,
    )
    .await);
    assert!(swept.deleted.is_empty(), "{swept:?}");
    assert_eq!(
        ok(env.world.index.unplaced(env.repo).await).len(),
        report.unplaced.len()
    );

    // The tag of a tree no commit has survives a full repack of old packs.
    env.age(100 * DAY).await;
    let lease = env.lease("node-r").await;
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(
        closure_present(env, l.tag).await,
        "the unplaced tag keeps its tree"
    );
    assert!(has_object(&env.node, env.repo, l.tagged_tree).await);

    // A dry run changes nothing and labels what it would release.
    assert_eq!(
        DISCARD_COMMITTED_WARNING,
        "deleted at the next full repack, with no grace period; this cannot be undone"
    );
    let dry = ok(reindex_discard(&env.node, env.repo, true).await);
    assert!(dry.dry_run);
    let dry_packs: BTreeSet<&str> = dry
        .recovered_packs
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(dry_packs, recovered);
    assert!(dry.committed_objects >= 3, "{dry:?}");
    assert!(dry.committed_bytes > 0, "{dry:?}");
    assert_eq!(
        ok(env.world.index.unplaced(env.repo).await).len(),
        report.unplaced.len()
    );
    assert_eq!(env.state(&l.p2).await, Some(PackState::Recovered));

    // Restore refuses what it can't do.
    for (oid, name) in [
        (l.c2, "refs/heads/main"),
        (Oid([0xab; 20]), "refs/heads/nothing"),
        (l.tag, "refs/heads/tag-branch"),
        (l.c2, "not-a-ref"),
    ] {
        let refused = reindex_restore(&env.node, env.repo, oid, name).await;
        assert!(
            matches!(refused, Err(GitError::Restore { .. })),
            "{name}: {refused:?}"
        );
    }

    // Restore a commit and a tag by its id: fetchable, entries cleared.
    ok(reindex_restore(&env.node, env.repo, l.c2, "refs/heads/restored").await);
    assert_eq!(env.db_ref("refs/heads/restored").await, Some(l.c2));
    assert_eq!(env.state(&l.p2).await, Some(PackState::Committed));
    assert!(!r22_refuses(&env.node, env.repo, l.c2).await, "fetchable");
    ok(reindex_restore(&env.node, env.repo, l.tag, "refs/tags/restored").await);
    assert!(!r22_refuses(&env.node, env.repo, l.tag).await);
    let left: Vec<UnplacedKind> = ok(env.world.index.unplaced(env.repo).await)
        .into_iter()
        .map(|u| u.kind)
        .collect();
    for gone in [
        UnplacedKind::Commit(l.c2),
        UnplacedKind::Tag(l.tag),
        UnplacedKind::Edge(l.c1),
    ] {
        assert!(!left.contains(&gone), "{gone:?} cleared: {left:?}");
    }
    assert!(left.contains(&UnplacedKind::Commit(l.c3)));
    assert_i1(&env.node, env.repo).await;

    // Discard ends recovery: the rest goes lost, every entry goes.
    let discarded = ok(reindex_discard(&env.node, env.repo, false).await);
    assert_eq!(discarded.recovered_packs.len(), 1);
    assert_eq!(env.state(&l.p3).await, Some(PackState::Lost));
    assert!(ok(env.world.index.unplaced(env.repo).await).is_empty());
    assert_i1(&env.node, env.repo).await;
    drop(l.old_db);
}

/// `reindex_hold_never_blocks_repack` (ADR 0004 item 4, ruling R24): with
/// unresolved entries, full repacks run and publish; every unplaced commit
/// in a committed pack and every edge object keeps its closure, no
/// recovered pack is an input, and a restore after several full repacks
/// yields a complete, fetchable closure.
#[tokio::test(flavor = "multi_thread")]
async fn reindex_hold_never_blocks_repack() {
    let l = lagging().await;
    let env = &l.env;
    let report = ok(reindex_repo(&env.node, env.repo, &env.lease("node-r").await).await);
    let edges: Vec<Oid> = report
        .unplaced
        .iter()
        .filter_map(|u| match u.kind {
            UnplacedKind::Edge(o) => Some(o),
            _ => None,
        })
        .collect();
    assert!(!edges.is_empty());
    env.age(100 * DAY).await;
    let lease = env.lease("node-r").await;
    let mut tip = l.c1;
    for i in 0..3 {
        tip = env.new_commit(tip, &format!("round/{i}.txt"), b"round\n");
        assert!(env.push("refs/heads/main", tip).await.ok("refs/heads/main"));
        let repack = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
        assert!(!repack.published.is_empty(), "round {i} published");
        for p in &repack.superseded {
            assert!(
                p.name != l.p2 && p.name != l.p3,
                "a recovered pack was an input"
            );
        }
        env.age(100 * DAY).await;
    }
    assert!(
        closure_present(env, l.c4).await,
        "the unplaced commit in a committed pack"
    );
    for edge in &edges {
        assert!(closure_present(env, *edge).await, "edge {edge}");
    }
    assert_eq!(env.state(&l.p2).await, Some(PackState::Recovered));

    ok(reindex_restore(&env.node, env.repo, l.c4, "refs/heads/side").await);
    ok(reindex_restore(&env.node, env.repo, l.c2, "refs/heads/c2").await);
    for oid in [l.c2, l.c4] {
        assert!(
            !r22_refuses(&env.node, env.repo, oid).await,
            "{oid} fetchable"
        );
    }
    let fresh = env.engine("node-fresh").await;
    assert_i1(&fresh, env.repo).await;
    push_support::fsck(&fresh, env.repo).await;
}

/// `recovered_packs_never_exposed` (ADR 0004 item 4): a recovered pack is
/// never linked into a snapshot on any node, its objects are not served,
/// a push can't build on them, it is never a repack input, and GC never
/// collects it.
#[tokio::test(flavor = "multi_thread")]
async fn recovered_packs_never_exposed() {
    let l = lagging().await;
    let env = &l.env;
    ok(reindex_repo(&env.node, env.repo, &env.lease("node-r").await).await);
    let fresh = env.engine("node-fresh").await;
    for engine in [&env.node, &fresh] {
        let snap = ok(engine.snapshot(env.repo).await);
        let linked: Vec<String> = ok(std::fs::read_dir(snap.dir.path().join("objects/pack")))
            .map(|e| ok(e).file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            !linked
                .iter()
                .any(|n| n.starts_with(&l.p2) || n.starts_with(&l.p3)),
            "{linked:?}"
        );
        assert!(r22_refuses(engine, env.repo, l.c2).await);
        assert!(!has_object(engine, env.repo, l.c2).await);
    }

    let w = env.work();
    let child = push_support::commit_tree(&w, &l.c1.to_string(), &[l.c2], &[]);
    let old = env.db_ref("refs/heads/main").await;
    let r = raw_push(
        &env.node,
        env.repo,
        "pusher",
        &[(old, Some(child), "refs/heads/main")],
        Some(pack_of(&w, &[child])),
    )
    .await;
    assert!(!r.ok("refs/heads/main"), "{r:?}");
    assert_eq!(env.db_ref("refs/heads/main").await, old);

    let lease = env.lease("node-r").await;
    let geometric = ok(repack_geometric(&env.node, env.repo, &lease).await);
    let full = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    for p in geometric.superseded.iter().chain(&full.superseded) {
        assert!(p.name != l.p2 && p.name != l.p3, "{p:?}");
    }
    ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now() + 365 * DAY,
        false,
    )
    .await);
    assert_eq!(env.state(&l.p2).await, Some(PackState::Recovered));
    assert_eq!(env.state(&l.p3).await, Some(PackState::Recovered));
    let files = env.bucket_files().await;
    assert!(files.iter().any(|f| f.contains(&l.p2)), "{files:?}");
}

/// `reindex discard` (ADR 0004 item 4) releases what only the unplaced
/// entries kept in committed packs: a full repack before the discard keeps
/// those objects, the first full repack after it drops them.
#[tokio::test(flavor = "multi_thread")]
async fn discard_releases_committed_objects() {
    let l = lagging().await;
    let env = &l.env;
    ok(reindex_repo(&env.node, env.repo, &env.lease("node-r").await).await);
    env.age(100 * DAY).await;
    let lease = env.lease("node-r").await;
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(
        has_object(&env.node, env.repo, l.c4).await,
        "kept by its entry"
    );
    assert!(has_object(&env.node, env.repo, l.tagged_tree).await);

    let started = now();
    let discarded = ok(reindex_discard(&env.node, env.repo, false).await);
    assert!(discarded.committed_objects >= 3, "{discarded:?}");
    // The discarded packs' grace period runs from the discard, not from
    // when reindex found them.
    for name in [&l.p2, &l.p3] {
        let row = env
            .rows()
            .await
            .into_iter()
            .find(|r| &r.pack.name == name)
            .unwrap_or_else(|| panic!("{name} listed"));
        assert_eq!(row.pack.state, PackState::Lost);
        assert!(row.inserted_at >= started, "{row:?}");
    }
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(!has_object(&env.node, env.repo, l.c4).await, "released");
    assert!(!has_object(&env.node, env.repo, l.tagged_tree).await);
    assert!(has_object(&env.node, env.repo, l.c1).await, "main keeps c1");
    assert_i1(&env.node, env.repo).await;
    drop(l.old_db);
}

/// A rebuilt ref falls back only to values since it was last created (ADR
/// 0004 item 4): a ref deleted and created again whose new value's closure
/// is gone from the bucket is left out, not rebuilt at a value of its
/// earlier lifetime.
#[tokio::test(flavor = "multi_thread")]
async fn ref_fallback_stops_at_recreation() {
    let env = env(12).await;
    let base = env.oid("refs/remotes/origin/main");
    let first = env.new_commit(base, "first.txt", b"first\n");
    assert!(env.push("refs/heads/x", first).await.ok("refs/heads/x"));
    assert!(env.delete("refs/heads/x").await.ok("refs/heads/x"));
    let before = env.committed_names().await;
    let second = env.new_commit(base, "second.txt", b"second\n");
    assert!(env.push("refs/heads/x", second).await.ok("refs/heads/x"));
    ok(derive_bucket_ref_log(&env.node, env.repo).await);
    let lost: Vec<String> = env
        .committed_names()
        .await
        .difference(&before)
        .cloned()
        .collect();
    assert_eq!(lost.len(), 1);
    // The recreated value's pack is gone from the bucket.
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    for ext in [PackExt::Pack, PackExt::Idx, PackExt::Rev] {
        let key = Key::Pack {
            repo: hex,
            name: ok(PackName::parse(&lost[0])),
            ext,
        };
        ok(env.world.bucket.delete(&key).await);
    }

    let report = ok(reindex_repo(&env.node, env.repo, &env.lease("node-a").await).await);
    assert_eq!(env.db_ref("refs/heads/x").await, None, "{report:?}");
    assert!(
        report
            .rolled_back
            .iter()
            .any(|r| r.name == "refs/heads/x" && r.logged == second && r.now.is_none()),
        "{report:?}"
    );
    assert!(env.db_ref("refs/heads/main").await.is_some());
    assert_i1(&env.node, env.repo).await;
}

/// `reindex` holds the repository's repack lease (ADR 0004 item 4), so it
/// excludes repacks and pack sweeps: with a lease another holder took over
/// it changes nothing, and while it holds the lease no sweep or repack can
/// take it.
#[tokio::test(flavor = "multi_thread")]
async fn reindex_excludes_repacks_and_sweeps() {
    let env = env(12).await;
    let stale = env.lease("node-a").await;
    let holder = env.take_over("node-b").await;
    let before = ok(env.world.index.snapshot(env.repo).await);
    let refused = reindex_repo(&env.node, env.repo, &stale).await;
    assert!(matches!(refused, Err(GitError::LeaseLost)), "{refused:?}");
    assert_eq!(
        ok(env.world.index.snapshot(env.repo).await),
        before,
        "nothing changed"
    );

    ok(reindex_repo(&env.node, env.repo, &holder).await);
    assert_eq!(
        ok(env
            .world
            .index
            .acquire_lease(env.repo, "gc", std::time::Duration::from_mins(10))
            .await),
        None,
        "the lease stays with reindex's holder"
    );
    let sweep = sweep_packs(&env.node, env.repo, &stale, DEFAULT_GRACE, now(), false).await;
    assert!(matches!(sweep, Err(GitError::LeaseLost)), "{sweep:?}");
    assert_i1(&env.node, env.repo).await;
}
