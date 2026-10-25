//! Repacks, the repack lease and fence, the pack sweep and the bucket ref
//! log (ADR 0004 items 3 to 5 and the checklist at its end).

mod common;
mod lifecycle_support;
mod push_support;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_git::{
    DEFAULT_GRACE, Engine, FaultPoint, GitError, PackFiles, Publication, PublishError, Publisher,
    derive_bucket_ref_log, git_command, maintain, prepare_full, prepare_geometric, repack_full,
    repack_geometric, sweep_packs,
};
use openxet_index::{CommitError, CommitTx, GitStore as _, Oid, PackState};
use openxet_storage::{Bucket as _, Key, PackExt, PackName, RepoIdHex};

use common::{World, fixture_repo, ok, pack_objects, rev, set};
use lifecycle_support::{DAY, Env, HubWrite, assert_i1, env, has_object, r22_refuses};

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

const GRACE_S: u64 = 30 * DAY;

/// An environment with three pushes to `main` on top of the seed: four
/// committed packs.
async fn pushed(commits: u32) -> (Env, Vec<Oid>) {
    let env = env(commits).await;
    let mut tips = Vec::new();
    let mut tip = env.oid("refs/remotes/origin/main");
    for i in 0..3 {
        tip = env.new_commit(tip, &format!("pushed/{i}.txt"), format!("{i}\n").as_bytes());
        let res = env.push("refs/heads/main", tip).await;
        assert!(res.ok("refs/heads/main"), "{res:?}");
        tips.push(tip);
    }
    (env, tips)
}

/// Every object in `engine`'s fresh snapshot of the repository.
async fn all_objects(engine: &Engine, env: &Env) -> BTreeSet<String> {
    let snap = ok(engine.snapshot(env.repo).await);
    let out = ok(git_command(engine.git(), &snap, None)
        .args([
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ])
        .output()
        .await);
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// `repack_publish_lease_fenced` (ADR 0004 item 5): a leaseholder whose
/// lease expired or was taken over can't publish, geometric or full, and
/// its pending outputs end `lost`, or `expired` after a crash; a publish
/// whose inputs are no longer all committed aborts.
#[tokio::test(flavor = "multi_thread")]
async fn repack_publish_lease_fenced() {
    let (env, _) = pushed(12).await;
    let before = env.committed_names().await;

    // Taken over between the working copy and the publish.
    let a = env.lease("node-a").await;
    let geometric = ok(prepare_geometric(&env.node, env.repo, &a).await);
    let _b = env.take_over("node-b").await;
    let fenced = geometric.publish(&env.node).await;
    assert!(matches!(fenced, Err(GitError::RepackFenced)), "{fenced:?}");

    let c = env.take_over("node-c").await;
    let full = ok(prepare_full(&env.node, env.repo, &c, DEFAULT_GRACE).await);
    let _d = env.take_over("node-d").await;
    let fenced = full.publish(&env.node).await;
    assert!(matches!(fenced, Err(GitError::RepackFenced)), "{fenced:?}");

    // Expired with no new holder.
    ok(sqlx::query("UPDATE repo_leases SET expires_at = 0")
        .execute(&env.world.sql().await)
        .await);
    let stale = env
        .world
        .index
        .acquire_lease(env.repo, "node-e", Duration::ZERO)
        .await;
    let e = ok(stale).unwrap_or_else(|| panic!("an expired lease is free"));
    let fenced = repack_geometric(&env.node, env.repo, &e).await;
    assert!(matches!(fenced, Err(GitError::RepackFenced)), "{fenced:?}");

    let rows = env.rows().await;
    let lost: Vec<_> = rows
        .iter()
        .filter(|r| !before.contains(&r.pack.name))
        .collect();
    assert!(!lost.is_empty(), "the fenced repacks wrote outputs");
    assert!(
        lost.iter().all(|r| r.pack.state == PackState::Lost),
        "{lost:?}"
    );
    assert_eq!(env.committed_names().await, before, "nothing was published");

    // Inputs that are no longer committed abort the publish.
    let f = env.take_over("node-f").await;
    let first = ok(prepare_geometric(&env.node, env.repo, &f).await);
    let second = ok(repack_geometric(&env.node, env.repo, &f).await);
    assert!(!second.published.is_empty(), "the second repack published");
    let fenced = first.publish(&env.node).await;
    assert!(matches!(fenced, Err(GitError::RepackFenced)), "{fenced:?}");

    // A repack that dies before its publish leaves pending outputs, which
    // GC expires after the grace period.
    let crashed = ok(prepare_full(&env.node, env.repo, &f, DEFAULT_GRACE).await);
    drop(crashed);
    let pending: Vec<String> = env
        .rows()
        .await
        .into_iter()
        .filter(|r| r.pack.state == PackState::Pending)
        .map(|r| r.pack.name)
        .collect();
    assert!(!pending.is_empty());
    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now() + GRACE_S + 60,
        false,
    )
    .await);
    for name in &pending {
        assert!(swept.expired.contains(name), "{name} expired: {swept:?}");
    }
    assert_i1(&env.node, env.repo).await;
}

/// `geometric_repack_drops_nothing_and_takes_no_fence` (ADR 0004 item 5):
/// every object of the inputs, unreachable ones included, is in the
/// outputs; the generation stays; a ref change during the repack doesn't
/// stop its publish, and a writer whose snapshot predates it still commits.
#[tokio::test(flavor = "multi_thread")]
async fn geometric_repack_drops_nothing_and_takes_no_fence() {
    let (env, tips) = pushed(12).await;
    // An unreachable commit in a committed pack.
    let main = env.db_ref("refs/heads/main").await;
    let res = env.push("refs/heads/main", tips[1]).await;
    assert!(res.ok("refs/heads/main"), "{res:?}");
    let objects = all_objects(&env.node, &env).await;
    let generation = ok(env.world.index.snapshot(env.repo).await).full_repack_gen;

    let lease = env.lease("node-a").await;
    let prepared = ok(prepare_geometric(&env.node, env.repo, &lease).await);
    // A ref moves while the repack runs: a geometric publish takes no fence.
    let next = env.new_commit(tips[1], "during.txt", b"during\n");
    let res = env.push("refs/heads/main", next).await;
    assert!(res.ok("refs/heads/main"), "{res:?}");
    let report = ok(prepared.publish(&env.node).await);
    assert!(!report.published.is_empty(), "{report:?}");
    assert!(report.superseded.len() >= 2, "{report:?}");
    assert!(report.midx_gen.is_some(), "{report:?}");

    let snap = ok(env.world.index.snapshot(env.repo).await);
    assert_eq!(snap.full_repack_gen, generation, "no generation bump");
    let after = all_objects(&env.node, &env).await;
    assert!(objects.is_subset(&after), "nothing was dropped");
    assert!(has_object(&env.node, env.repo, main.unwrap_or(tips[2])).await);

    // A writer whose snapshot read the generation before the publish.
    let w = HubWrite {
        engine: &env.node,
        repo: env.repo,
        update: set("refs/heads/kept", None, tips[0]),
    };
    let checked = ok(w.check().await);
    let lease = env.take_over("node-b").await;
    ok(repack_geometric(&env.node, env.repo, &lease).await);
    ok(w.commit(&env.world, &checked).await);
    assert_eq!(env.db_ref("refs/heads/kept").await, Some(tips[0]));
    assert_i1(&env.node, env.repo).await;
    let fresh = env.engine("node-fresh").await;
    assert_i1(&fresh, env.repo).await;
}

/// `full_repack_fence_both_directions` (ADR 0004 item 5): a full repack's
/// publish bumps the generation, so a writer whose snapshot read the old
/// one fails and its pack ends `lost`; and a full repack whose working copy
/// predates a committed ref change aborts, and succeeds when run again.
#[tokio::test(flavor = "multi_thread")]
async fn full_repack_fence_both_directions() {
    let (env, tips) = pushed(12).await;
    let generation = ok(env.world.index.snapshot(env.repo).await).full_repack_gen;

    // Repack after push: the ref log moves past the high-water mark.
    let lease = env.lease("node-a").await;
    let prepared = ok(prepare_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    let next = env.new_commit(tips[2], "late.txt", b"late\n");
    let res = env.push("refs/heads/main", next).await;
    assert!(res.ok("refs/heads/main"), "{res:?}");
    let fenced = prepared.publish(&env.node).await;
    assert!(matches!(fenced, Err(GitError::RepackFenced)), "{fenced:?}");
    assert_eq!(
        ok(env.world.index.snapshot(env.repo).await).full_repack_gen,
        generation
    );
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(!report.published.is_empty());
    assert_eq!(
        ok(env.world.index.snapshot(env.repo).await).full_repack_gen,
        generation + 1
    );

    // Push after repack: a writer with the old generation fails.
    let snap = ok(env.world.index.snapshot(env.repo).await);
    let src = fixture_repo(&env.fx);
    let files = pack_objects(&src, "main~1\n^main~2", &env.world.dir.path().join("late"));
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    let result = Publisher::new(env.world.index.clone(), env.world.bucket.clone())
        .publish(Publication {
            repo: env.repo,
            pack: Some(&PackFiles {
                dir: files.dir.clone(),
                name: ok(PackName::parse(&files.name)),
            }),
            updates: &[set("refs/heads/old-gen", None, rev(&src, "main~1"))],
            full_repack_gen: snap.full_repack_gen,
            actor: "writer",
            request: "r",
        })
        .await;
    assert!(
        matches!(result, Err(PublishError::Commit(CommitError::Fenced))),
        "{result:?}"
    );
    assert_eq!(env.state(&files.name).await, Some(PackState::Lost));
    assert_eq!(env.db_ref("refs/heads/old-gen").await, None);
    assert_i1(&env.node, env.repo).await;
}

/// `keep_refs_cover_ref_log_window` (ADR 0004 item 5): a commit force-pushed
/// away less than a grace period ago survives a full repack whatever its
/// pack's age; once the ref log row leaves the window, the next full repack
/// drops it.
#[tokio::test(flavor = "multi_thread")]
async fn keep_refs_cover_ref_log_window() {
    let (env, tips) = pushed(12).await;
    let away = tips[2];
    let res = env.push("refs/heads/main", tips[0]).await;
    assert!(res.ok("refs/heads/main"), "{res:?}");
    // Every pack looks a year old; only the ref log row is recent.
    env.age(0).await;
    ok(
        sqlx::query("UPDATE packs SET created_at = 1700000000 WHERE repo = ?")
            .bind(&env.repo.0[..])
            .execute(&env.world.sql().await)
            .await,
    );

    let lease = env.lease("node-a").await;
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(report.keep_refs >= 1, "{report:?}");
    assert!(
        has_object(&env.node, env.repo, away).await,
        "kept by the ref log window"
    );
    assert!(
        r22_refuses(&env.node, env.repo, away).await,
        "but never served"
    );

    env.age(GRACE_S + DAY).await;
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(!report.published.is_empty());
    assert!(
        !has_object(&env.node, env.repo, away).await,
        "dropped once out of the window"
    );
    assert_i1(&env.node, env.repo).await;
}

/// `hydrated_pack_mtime_pinned`, the leaseholder's half (ADR 0004 item 5):
/// the repack pins every input pack's mtime to its row right before git
/// runs, so a cached pack git freshened still expires its unreachable
/// objects by the pack's creation time. The hook freshens the cached files
/// after the working copy's snapshot is built, as serving git would, so
/// only the leaseholder's own re-pin can set them back.
#[tokio::test(flavor = "multi_thread")]
async fn hydrated_pack_mtime_pinned() {
    let (env, tips) = pushed(12).await;
    let res = env.push("refs/heads/main", tips[0]).await;
    assert!(res.ok("refs/heads/main"), "{res:?}");
    env.age(GRACE_S + DAY).await;
    let cache = env
        .world
        .dir
        .path()
        .join("node-a/repos")
        .join(env.repo.to_string());
    let snap = ok(env.world.index.snapshot(env.repo).await);
    let files: Vec<std::path::PathBuf> = snap
        .packs
        .iter()
        .map(|p| cache.join(format!("{}.pack", p.name)))
        .collect();
    let freshened = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&freshened);
    env.node.set_fault_hook(
        FaultPoint::RepackBeforeGit,
        Some(Arc::new(move |_| {
            for file in &files {
                let f = ok(std::fs::File::open(file));
                ok(f.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now())));
            }
            flag.store(true, Ordering::SeqCst);
            Box::pin(async {})
        })),
    );

    let lease = env.lease("node-a").await;
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    env.node.set_fault_hook(FaultPoint::RepackBeforeGit, None);
    assert!(freshened.load(Ordering::SeqCst), "the hook ran");
    for p in &snap.packs {
        let file = cache.join(format!("{}.pack", p.name));
        let mtime = ok(ok(std::fs::metadata(&file)).modified());
        assert_eq!(
            mtime,
            UNIX_EPOCH + Duration::from_secs(p.created_at),
            "{} pinned again",
            p.name
        );
    }
    assert!(
        !has_object(&env.node, env.repo, tips[2]).await,
        "the freshened pack's old unreachable commit expired"
    );
    assert_i1(&env.node, env.repo).await;
}

/// `mtimes_rev_midx_travel_through_bucket` (ADR 0004 items 4 and 5): a
/// full repack's cruft pack uploads its `.mtimes`, a geometric repack its
/// multi-pack index and bitmap, and a node with an empty cache hydrates and
/// links every one of them.
#[tokio::test(flavor = "multi_thread")]
async fn mtimes_rev_midx_travel_through_bucket() {
    let (env, tips) = pushed(12).await;
    let res = env.push("refs/heads/main", tips[0]).await;
    assert!(res.ok("refs/heads/main"), "{res:?}");
    let lease = env.lease("node-a").await;
    env.age(GRACE_S + DAY).await;
    // The ref log window has passed, but the packs' age doesn't pass the
    // cruft expiration for an object that became unreachable recently: put
    // the recent push's time back so its commit lands in a cruft pack.
    ok(
        sqlx::query("UPDATE packs SET created_at = ? WHERE repo = ? AND created_at > ?")
            .bind(i64::try_from(now()).unwrap_or(i64::MAX))
            .bind(&env.repo.0[..])
            .bind(1_761_400_000_i64)
            .execute(&env.world.sql().await)
            .await,
    );
    let full = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    let mut cruft = 0;
    for p in &full.published {
        let key = Key::Pack {
            repo: hex,
            name: ok(PackName::parse(&p.name)),
            ext: PackExt::Mtimes,
        };
        if ok(env.world.bucket.size(&key).await).is_some() {
            cruft += 1;
        }
    }
    assert_eq!(cruft, 1, "one cruft pack with its .mtimes: {full:?}");

    let next = env.new_commit(tips[0], "after.txt", b"after\n");
    assert!(
        env.push("refs/heads/main", next)
            .await
            .ok("refs/heads/main")
    );
    let next2 = env.new_commit(next, "after2.txt", b"after2\n");
    assert!(
        env.push("refs/heads/main", next2)
            .await
            .ok("refs/heads/main")
    );
    let geometric = ok(repack_geometric(&env.node, env.repo, &lease).await);
    let generation = geometric
        .midx_gen
        .unwrap_or_else(|| panic!("a multi-pack index: {geometric:?}"));
    let files = env.bucket_files().await;
    let midx = format!("repos/{hex}/midx/{generation}.multi-pack-index");
    assert!(files.contains(&midx), "{files:?}");
    assert!(
        files.contains(&format!("repos/{hex}/midx/{generation}.bitmap")),
        "{files:?}"
    );

    let fresh = env.engine("node-fresh").await;
    let snap = ok(fresh.snapshot(env.repo).await);
    let linked: BTreeSet<String> = ok(std::fs::read_dir(snap.dir.path().join("objects/pack")))
        .map(|e| ok(e).file_name().to_string_lossy().into_owned())
        .collect();
    for p in &snap.packs {
        for f in &files {
            if let Some(ext) = f.strip_prefix(&format!("repos/{hex}/packs/{}.", p.name)) {
                assert!(
                    linked.contains(&format!("{}.{ext}", p.name)),
                    "{f} linked: {linked:?}"
                );
            }
        }
    }
    assert!(linked.contains("multi-pack-index"), "{linked:?}");
    assert!(
        linked
            .iter()
            .any(|n| n.starts_with("multi-pack-index-") && n.ends_with(".bitmap")),
        "{linked:?}"
    );
    assert!(linked.iter().any(|n| n.ends_with(".mtimes")), "{linked:?}");
    assert_i1(&fresh, env.repo).await;
}

/// `gc_removes_pack_side_files` (ADR 0004 item 5, design §5.6): after the
/// sweep deletes a pack, none of its side files remain, and a multi-pack
/// index generation goes once it has not been current for the grace
/// period.
#[tokio::test(flavor = "multi_thread")]
async fn gc_removes_pack_side_files() {
    let (env, tips) = pushed(12).await;
    let lease = env.lease("node-a").await;
    let g1 = ok(repack_geometric(&env.node, env.repo, &lease).await)
        .midx_gen
        .unwrap_or_else(|| panic!("generation 1"));
    let mut tip = tips[2];
    for i in 0..3 {
        tip = env.new_commit(tip, &format!("more/{i}.txt"), b"more\n");
        assert!(env.push("refs/heads/main", tip).await.ok("refs/heads/main"));
    }
    let g2 = ok(repack_geometric(&env.node, env.repo, &lease).await)
        .midx_gen
        .unwrap_or_else(|| panic!("generation 2"));
    assert!(g2 > g1);
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    let has_gen = |files: &BTreeSet<String>, g: u64| {
        files.contains(&format!("repos/{hex}/midx/{g}.multi-pack-index"))
    };

    let early = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now(),
        false,
    )
    .await);
    assert!(
        early.deleted.is_empty() && early.midx_deleted.is_empty(),
        "{early:?}"
    );

    let later = now() + GRACE_S + 60;
    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        later,
        false,
    )
    .await);
    assert!(!swept.deleted.is_empty(), "{swept:?}");
    assert_eq!(swept.midx_deleted, vec![g1]);
    let files = env.bucket_files().await;
    assert!(!has_gen(&files, g1) && has_gen(&files, g2), "{files:?}");

    // A full repack leaves no committed pack carrying a generation.
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        later,
        false,
    )
    .await);
    assert_eq!(swept.midx_deleted, vec![g2], "{swept:?}");

    let committed = env.committed_names().await;
    let files = env.bucket_files().await;
    for f in &files {
        let name = f
            .strip_prefix(&format!("repos/{hex}/packs/"))
            .and_then(|n| n.split('.').next())
            .unwrap_or_else(|| panic!("only pack files remain: {f}"));
        assert!(committed.contains(name), "{f} belongs to a committed pack");
    }
    assert_i1(&env.node, env.repo).await;
}

/// `sweep_deletes_only_past_grace` (ADR 0004 item 5): superseded, lost and
/// expired packs go only past their grace period, committed and recovered
/// packs never, a dry run changes nothing, and a grace below 28 days is
/// refused.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn sweep_deletes_only_past_grace() {
    let (env, _) = pushed(12).await;
    let src = fixture_repo(&env.fx);
    let out = env.world.dir.path().join("extra");
    let lost = env
        .world
        .pending(
            env.repo,
            &pack_objects(&src, "main~3\n^main~4", &out.join("l")),
            now(),
        )
        .await;
    assert!(ok(env
        .world
        .index
        .mark_pack(lost.id, PackState::Pending, PackState::Lost)
        .await));
    let recovered = env
        .world
        .pending(
            env.repo,
            &pack_objects(&src, "main~5\n^main~6", &out.join("r")),
            1_700_000_000,
        )
        .await;
    ok(
        sqlx::query("UPDATE packs SET state = 'recovered' WHERE id = ?")
            .bind(&recovered.id.0[..])
            .execute(&env.world.sql().await)
            .await,
    );
    let lease = env.lease("node-a").await;
    let report = ok(repack_geometric(&env.node, env.repo, &lease).await);
    let superseded: Vec<String> = report.superseded.iter().map(|p| p.name.clone()).collect();
    assert!(!superseded.is_empty());
    let files = env.bucket_files().await;

    let short = sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        Duration::from_secs(27 * DAY),
        now(),
        false,
    )
    .await;
    assert!(
        matches!(short, Err(GitError::GraceTooShort { .. })),
        "{short:?}"
    );

    let just_before = now() + GRACE_S - 3600;
    let none = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        just_before,
        false,
    )
    .await);
    assert!(none.deleted.is_empty(), "{none:?}");
    assert_eq!(env.bucket_files().await, files);

    let far = now() + 10 * GRACE_S;
    let dry = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        far,
        true,
    )
    .await);
    let dry_names: BTreeSet<String> = dry.deleted.iter().map(|d| d.name.clone()).collect();
    let mut expected: BTreeSet<String> = superseded.iter().cloned().collect();
    expected.insert(lost.name.clone());
    assert_eq!(dry_names, expected);
    assert_eq!(env.bucket_files().await, files, "a dry run deletes nothing");
    assert_eq!(env.state(&lost.name).await, Some(PackState::Lost));

    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        far,
        false,
    )
    .await);
    let names: BTreeSet<String> = swept.deleted.iter().map(|d| d.name.clone()).collect();
    assert_eq!(names, expected);
    assert_eq!(env.state(&recovered.name).await, Some(PackState::Recovered));
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    let left = env.bucket_files().await;
    assert!(left.contains(&format!("repos/{hex}/packs/{}.pack", recovered.name)));
    for name in &expected {
        assert!(
            !left.iter().any(|f| f.contains(name.as_str())),
            "{name} gone"
        );
    }
    for p in ok(env.world.index.snapshot(env.repo).await).packs {
        assert!(left.contains(&format!("repos/{hex}/packs/{}.pack", p.name)));
    }
    assert_i1(&env.node, env.repo).await;
}

/// `lost_packs_not_hydrated` (ADR 0004): a lost pack's files stay in the
/// bucket until GC, but no node downloads them; the lost update matches
/// only `pending`.
#[tokio::test(flavor = "multi_thread")]
async fn lost_packs_not_hydrated() {
    let (env, _) = pushed(12).await;
    let src = fixture_repo(&env.fx);
    let files = pack_objects(&src, "main~3\n^main~4", &env.world.dir.path().join("lost"));
    let row = env.world.pending(env.repo, &files, now()).await;
    assert!(ok(env
        .world
        .index
        .mark_pack(row.id, PackState::Pending, PackState::Lost)
        .await));
    assert!(!ok(env
        .world
        .index
        .mark_pack(row.id, PackState::Pending, PackState::Lost)
        .await));
    let committed = ok(env.world.index.snapshot(env.repo).await).packs[0].id;
    assert!(!ok(env
        .world
        .index
        .mark_pack(committed, PackState::Pending, PackState::Lost)
        .await));

    let fresh = env.engine("node-fresh").await;
    drop(ok(fresh.snapshot(env.repo).await));
    let cache = env
        .world
        .dir
        .path()
        .join("node-fresh/repos")
        .join(env.repo.to_string());
    assert!(!cache.join(format!("{}.pack", files.name)).exists());
    assert!(!cache.join(format!("{}.idx", files.name)).exists());
    let lease = env.lease("node-a").await;
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(
        report.superseded.iter().all(|p| p.id != row.id),
        "never an input"
    );
}

/// `pending_packs_aged_out_by_gc` (ADR 0004): a pending pack stays through
/// its grace period, then the sweep marks it `expired` (matching only
/// `pending`) and deletes it, and its late commit fails cleanly.
#[tokio::test(flavor = "multi_thread")]
async fn pending_packs_aged_out_by_gc() {
    let (env, _) = pushed(12).await;
    let src = fixture_repo(&env.fx);
    let files = pack_objects(
        &src,
        "main~3\n^main~4",
        &env.world.dir.path().join("pending"),
    );
    let row = env.world.pending(env.repo, &files, now()).await;
    let generation = ok(env.world.index.snapshot(env.repo).await).full_repack_gen;

    let early = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now() + GRACE_S - 60,
        false,
    )
    .await);
    assert!(
        early.expired.is_empty() && early.deleted.is_empty(),
        "{early:?}"
    );
    assert_eq!(env.state(&files.name).await, Some(PackState::Pending));

    let late = now() + GRACE_S + 60;
    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        late,
        false,
    )
    .await);
    assert_eq!(swept.expired, vec![files.name.clone()]);
    assert!(
        swept
            .deleted
            .iter()
            .any(|d| d.name == files.name && d.state == PackState::Expired)
    );
    assert_eq!(env.state(&files.name).await, None);
    assert!(
        !env.bucket_files()
            .await
            .iter()
            .any(|f| f.contains(&files.name))
    );

    let commit = env
        .world
        .index
        .commit(CommitTx {
            repo: env.repo,
            updates: &[set("refs/heads/late", None, rev(&src, "main~3"))],
            pack: Some(row.id),
            full_repack_gen: generation,
            actor: "late",
            request: "late",
        })
        .await;
    assert!(
        matches!(commit, Err(CommitError::PackNotPending)),
        "{commit:?}"
    );
    assert_eq!(env.db_ref("refs/heads/late").await, None);
    // Committed packs are never expired, however old.
    assert!(
        env.rows()
            .await
            .iter()
            .all(|r| r.pack.state == PackState::Committed)
    );
}

/// `push_cannot_use_superseded_objects` (ADR 0004 item 4): once a full
/// repack has dropped an expired object, a push that names it without
/// sending it is refused by receive-pack's connectivity check, though the
/// superseded inputs holding it are still in the bucket and the cache.
#[tokio::test(flavor = "multi_thread")]
async fn push_cannot_use_superseded_objects() {
    let (env, tips) = pushed(12).await;
    let gone = tips[2];
    assert!(
        env.push("refs/heads/main", tips[0])
            .await
            .ok("refs/heads/main")
    );
    env.age(GRACE_S + DAY).await;
    let lease = env.lease("node-a").await;
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(!report.superseded.is_empty());
    assert!(!has_object(&env.node, env.repo, gone).await);

    // A child of the dropped commit, sent alone.
    let w = env.work();
    let child = push_support::commit_tree(&w, &tips[0].to_string(), &[gone], &[]);
    let pack = push_support::pack_of(&w, &[child]);
    let old = env.db_ref("refs/heads/main").await;
    let res = push_support::raw_push(
        &env.node,
        env.repo,
        "pusher",
        &[(old, Some(child), "refs/heads/main")],
        Some(pack),
    )
    .await;
    assert!(!res.ok("refs/heads/main"), "{res:?}");
    assert_eq!(env.db_ref("refs/heads/main").await, old);
    assert_i1(&env.node, env.repo).await;
}

/// `ref_log_in_cas_transaction`, the derivation and retention half (ADR
/// 0004 item 3, design §5.3): the bucket ref log is derived from the
/// `ref_log` rows of every writer, each row once; rows are kept for the
/// grace period plus the longest a pack can stay pending, and only once the
/// bucket holds them.
#[tokio::test(flavor = "multi_thread")]
async fn ref_log_in_cas_transaction() {
    let (env, tips) = pushed(12).await;
    HubWrite {
        engine: &env.node,
        repo: env.repo,
        update: set("refs/heads/hub", None, tips[0]),
    }
    .run(&env.world)
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let rows = ok(env.world.index.ref_log_since(env.repo, 0).await);
    assert_eq!(
        ok(derive_bucket_ref_log(&env.node, env.repo).await),
        rows.len() as u64
    );
    assert_eq!(ok(derive_bucket_ref_log(&env.node, env.repo).await), 0);

    // Another node derives from where the bucket is.
    let other = env.engine("node-b").await;
    assert_eq!(ok(derive_bucket_ref_log(&other, env.repo).await), 0);
    assert!(env.delete("refs/heads/hub").await.ok("refs/heads/hub"));
    assert_eq!(ok(derive_bucket_ref_log(&other, env.repo).await), 1);

    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    let mut in_bucket = Vec::new();
    for key in ok(env
        .world
        .bucket
        .list_under(hex, openxet_storage::KeyKind::RefLog)
        .await)
    {
        let bytes = ok(env.world.bucket.get(&key).await).unwrap_or_default();
        in_bucket.extend(ok(openxet_storage::RefLogRecord::from_bytes(&key, &bytes)).entries);
    }
    in_bucket.sort_by_key(|e| e.seq);
    let rows = ok(env.world.index.ref_log_since(env.repo, 0).await);
    assert_eq!(in_bucket.len(), rows.len());
    for (e, r) in in_bucket.iter().zip(&rows) {
        assert_eq!(e.seq, r.seq);
        assert_eq!(e.ref_name, r.name);
        assert_eq!(e.old, r.old.map(|o| o.to_string()));
        assert_eq!(e.new, r.new.map(|o| o.to_string()));
        assert_eq!((e.actor.as_str(), e.at), (r.actor.as_str(), r.at));
    }

    // Retention: two grace periods, and only what the bucket holds.
    let undelivered = env.new_commit(tips[2], "undelivered.txt", b"u\n");
    assert!(
        env.push("refs/heads/main", undelivered)
            .await
            .ok("refs/heads/main")
    );
    let retained = now() + 2 * GRACE_S - 3600;
    let kept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        retained,
        false,
    )
    .await);
    assert_eq!(kept.ref_log_pruned, 0);
    let past = now() + 2 * GRACE_S + 3600;
    let pruned = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        past,
        false,
    )
    .await);
    assert_eq!(pruned.ref_log_pruned, rows.len() as u64);
    let left = ok(env.world.index.ref_log_since(env.repo, 0).await);
    assert_eq!(
        left.len(),
        1,
        "the row not yet in the bucket stays: {left:?}"
    );
    assert_eq!(left[0].new, Some(undelivered));
}

/// `publish_then_cas_survives_faults`, the repack and Hub ref cases (ADR
/// 0004): a full repack while a push sits between publish and commit; a
/// dependent push on a branch built on a commit force-pushed away; a push
/// building on a long-unreachable object; a push committing while a full
/// repack computes; and a Hub ref create held across a full repack and GC,
/// for a reachable and a long-unreachable target. No ref may point at a
/// missing object, no acknowledged write is lost, and a pending pack is
/// neither a repack input nor deleted before its grace period.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn publish_then_cas_survives_faults() {
    let (env, tips) = pushed(12).await;
    let lease = env.lease("node-a").await;
    let src = fixture_repo(&env.fx);

    // A push held between publish and commit while a full repack runs.
    let held = pack_objects(&src, "main~1\n^main~2", &env.world.dir.path().join("held"));
    let row = env.world.pending(env.repo, &held, now()).await;
    let generation = ok(env.world.index.snapshot(env.repo).await).full_repack_gen;
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(
        report.superseded.iter().all(|p| p.id != row.id),
        "not an input"
    );
    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now(),
        false,
    )
    .await);
    assert!(swept.deleted.iter().all(|d| d.name != held.name));
    let late = env
        .world
        .index
        .commit(CommitTx {
            repo: env.repo,
            updates: &[set("refs/heads/held", None, rev(&src, "main~1"))],
            pack: Some(row.id),
            full_repack_gen: generation,
            actor: "held",
            request: "held",
        })
        .await;
    assert!(matches!(late, Err(CommitError::Fenced)), "{late:?}");
    assert_eq!(env.db_ref("refs/heads/held").await, None);
    assert_i1(&env.node, env.repo).await;

    // Force-push then a dependent push: branch A at a1 is forced away while
    // a push to B building on a1 waits to commit.
    let a1 = env.new_commit(tips[2], "a1.txt", b"a1\n");
    assert!(env.push("refs/heads/a", a1).await.ok("refs/heads/a"));
    let b1 = env.new_commit(a1, "b1.txt", b"b1\n");
    let b_objects = [
        b1,
        env.oid(&format!("{b1}^{{tree}}")),
        env.oid(&format!("{b1}:b1.txt")),
    ];
    let b_pack = push_support::pack_of(&env.work(), &b_objects);
    let b_files = index_pack(&env.world.dir.path().join("b1"), &b_pack);
    let b_row = env.world.pending(env.repo, &b_files, now()).await;
    let b_gen = ok(env.world.index.snapshot(env.repo).await).full_repack_gen;
    assert!(env.push("refs/heads/a", tips[2]).await.ok("refs/heads/a"));
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now(),
        false,
    )
    .await);
    assert!(
        has_object(&env.node, env.repo, a1).await,
        "a1 is in the ref log window"
    );
    let b_commit = env
        .world
        .index
        .commit(CommitTx {
            repo: env.repo,
            updates: &[set("refs/heads/b", None, b1)],
            pack: Some(b_row.id),
            full_repack_gen: b_gen,
            actor: "b",
            request: "b",
        })
        .await;
    assert!(matches!(b_commit, Err(CommitError::Fenced)), "{b_commit:?}");
    assert!(ok(env
        .world
        .index
        .mark_pack(b_row.id, PackState::Pending, PackState::Lost)
        .await));
    assert_i1(&env.node, env.repo).await;

    // A held push building on a long-unreachable object fails on the fence
    // once a full repack drops that object.
    env.age(GRACE_S + DAY).await;
    let c1 = env.new_commit(a1, "c1.txt", b"c1\n");
    let c_pack = push_support::pack_of(&env.work(), &[c1, env.oid(&format!("{c1}^{{tree}}"))]);
    let c_files = index_pack(&env.world.dir.path().join("c1"), &c_pack);
    let c_row = env.world.pending(env.repo, &c_files, now()).await;
    let c_gen = ok(env.world.index.snapshot(env.repo).await).full_repack_gen;
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(!has_object(&env.node, env.repo, a1).await, "a1 expired");
    let c_commit = env
        .world
        .index
        .commit(CommitTx {
            repo: env.repo,
            updates: &[set("refs/heads/c", None, c1)],
            pack: Some(c_row.id),
            full_repack_gen: c_gen,
            actor: "c",
            request: "c",
        })
        .await;
    assert!(matches!(c_commit, Err(CommitError::Fenced)), "{c_commit:?}");
    assert_eq!(env.db_ref("refs/heads/c").await, None);

    // A push commits while a full repack computes: the repack aborts and
    // its rerun succeeds.
    let d0 = env.new_commit(tips[2], "d0.txt", b"d0\n");
    assert!(env.push("refs/heads/main", d0).await.ok("refs/heads/main"));
    let prepared = ok(prepare_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    let d1 = env.new_commit(d0, "d1.txt", b"d1\n");
    assert!(env.push("refs/heads/main", d1).await.ok("refs/heads/main"));
    let fenced = prepared.publish(&env.node).await;
    assert!(matches!(fenced, Err(GitError::RepackFenced)), "{fenced:?}");
    ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert_eq!(
        env.db_ref("refs/heads/main").await,
        Some(d1),
        "the push survived"
    );

    // A Hub ref create held across a full repack and GC: the reachable
    // target retries from a fresh snapshot and lands.
    let hub = HubWrite {
        engine: &env.node,
        repo: env.repo,
        update: set("refs/heads/from-hub", None, tips[0]),
    };
    let checked = ok(hub.check().await);
    let e1 = env.new_commit(d1, "e1.txt", b"e1\n");
    assert!(env.push("refs/heads/main", e1).await.ok("refs/heads/main"));
    let repack = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(!repack.published.is_empty());
    ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now() + GRACE_S + 60,
        false,
    )
    .await);
    let first = hub.commit(&env.world, &checked).await;
    assert!(
        matches!(first, Err(PublishError::Commit(CommitError::Fenced))),
        "{first:?}"
    );
    hub.run(&env.world).await.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(env.db_ref("refs/heads/from-hub").await, Some(tips[0]));

    // A long-unreachable target is refused before any transaction.
    let away = HubWrite {
        engine: &env.node,
        repo: env.repo,
        update: set("refs/tags/away", None, a1),
    };
    let refused = away.run(&env.world).await;
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(env.db_ref("refs/tags/away").await, None);
    assert_i1(&env.node, env.repo).await;
    let fresh = env.engine("node-fresh").await;
    assert_i1(&fresh, env.repo).await;
}

/// Indexes a raw pack into `dir` with `git index-pack`, as receive-pack
/// would.
fn index_pack(dir: &std::path::Path, pack: &[u8]) -> common::PackFiles {
    ok(std::fs::create_dir_all(dir));
    let tmp = dir.join("incoming.pack");
    ok(std::fs::write(&tmp, pack));
    let out = common::git(dir, &["index-pack", "incoming.pack"]);
    let sum = out.trim().trim_start_matches("pack\t").to_owned();
    let name = format!("pack-{sum}");
    ok(std::fs::rename(&tmp, dir.join(format!("{name}.pack"))));
    ok(std::fs::rename(
        dir.join("incoming.idx"),
        dir.join(format!("{name}.idx")),
    ));
    common::PackFiles {
        name,
        dir: dir.to_path_buf(),
    }
}

/// The server's background task (ADR 0004 item 5, design §5.3): a pass
/// derives every repository's bucket ref log and repacks each repository
/// with more than 50 committed packs, under the lease.
#[tokio::test(flavor = "multi_thread")]
async fn maintenance_derives_ref_log_and_repacks() {
    let world = World::new().await;
    let (busy, fx) = push_support::seeded(&world, 60).await;
    let (quiet, _qfx) = push_support::seeded(&world, 5).await;
    let src = fixture_repo(&fx);
    let out = world.dir.path().join("many");
    for i in 1..=50 {
        let files = pack_objects(
            &src,
            &format!("main~{i}\n^main~{}", i + 1),
            &out.join(i.to_string()),
        );
        world
            .commit(
                busy,
                &files,
                1_761_400_000 + i,
                &[set(
                    &format!("refs/heads/p{i}"),
                    None,
                    rev(&src, &format!("main~{i}")),
                )],
            )
            .await;
    }
    let node = push_support::engine(&world, push_support::config(&world, "node")).await;
    assert_eq!(ok(world.index.snapshot(busy).await).packs.len(), 51);

    let report = ok(maintain(&node, "node").await);
    assert_eq!(report.repos, 2);
    assert_eq!(report.repacked, vec![busy]);
    assert_eq!(report.failures, 0);
    let rows = ok(world.index.ref_log_since(busy, 0).await).len()
        + ok(world.index.ref_log_since(quiet, 0).await).len();
    assert_eq!(report.ref_log_rows, rows as u64);
    assert!(ok(world.index.snapshot(busy).await).packs.len() < 51);
    assert_eq!(ok(world.index.snapshot(quiet).await).packs.len(), 1);
    let again = ok(maintain(&node, "node").await);
    assert_eq!((again.ref_log_rows, again.repacked.len()), (0, 0));
    assert_i1(&node, busy).await;
}

/// A pack name is its checksum, so a superseded row and a committed one can
/// name the same files, as when a push is sent again after a repack
/// replaced its first pack: the sweep deletes the superseded row but keeps
/// the files the committed row needs.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_keeps_files_another_row_names() {
    let env = env(12).await;
    let base = env.oid("refs/remotes/origin/main");
    let tip = env.new_commit(base, "again.txt", b"again\n");
    let pack = env.pack_for(tip).await;
    let seed = env.committed_names().await;
    let first = push_support::raw_push(
        &env.node,
        env.repo,
        "pusher",
        &[(Some(base), Some(tip), "refs/heads/main")],
        Some(pack.clone()),
    )
    .await;
    assert!(first.ok("refs/heads/main"), "{first:?}");
    let pushed: Vec<String> = env
        .committed_names()
        .await
        .difference(&seed)
        .cloned()
        .collect();
    assert_eq!(pushed.len(), 1);
    let lease = env.lease("node-a").await;
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    assert!(
        report.superseded.iter().any(|p| p.name == pushed[0]),
        "{report:?}"
    );
    let again = push_support::raw_push(
        &env.node,
        env.repo,
        "pusher",
        &[(None, Some(tip), "refs/heads/again")],
        Some(pack),
    )
    .await;
    assert!(again.ok("refs/heads/again"), "{again:?}");
    let resent = env
        .rows()
        .await
        .into_iter()
        .find(|r| r.pack.state == PackState::Committed && r.pack.name == pushed[0])
        .unwrap_or_else(|| panic!("the resent pack is committed under its old name"));

    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now() + 2 * GRACE_S,
        false,
    )
    .await);
    assert!(
        swept.deleted.iter().any(|d| d.name == resent.pack.name),
        "{swept:?}"
    );
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    let key = Key::Pack {
        repo: hex,
        name: ok(PackName::parse(&resent.pack.name)),
        ext: PackExt::Pack,
    };
    assert!(
        ok(env.world.bucket.size(&key).await).is_some(),
        "the committed row's files stay"
    );
    let fresh = env.engine("node-fresh").await;
    assert_i1(&fresh, env.repo).await;
}

/// Repack outputs carry their oldest input's `created_at`, so in an aged
/// repository they look old by object age the moment they are written. The
/// sweep ages `pending` rows by `inserted_at` (ADR 0004 item 5), so a sweep
/// between a repack's prepare and its publish leaves the outputs pending
/// with their files, and the publish succeeds.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_during_repack_keeps_outputs() {
    let (env, _) = pushed(12).await;
    env.age(2 * GRACE_S).await;
    let lease = env.lease("node-a").await;
    let prepared = ok(prepare_geometric(&env.node, env.repo, &lease).await);
    let outputs: Vec<_> = env
        .rows()
        .await
        .into_iter()
        .filter(|r| r.pack.state == PackState::Pending)
        .collect();
    assert!(!outputs.is_empty(), "the repack wrote outputs");
    assert!(
        outputs.iter().all(|r| r.pack.created_at + GRACE_S < now()),
        "the outputs carry an object age past the grace period: {outputs:?}"
    );

    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &env.gc_lease().await,
        DEFAULT_GRACE,
        now(),
        false,
    )
    .await);
    assert!(swept.expired.is_empty(), "{swept:?}");
    for r in &outputs {
        assert_eq!(env.state(&r.pack.name).await, Some(PackState::Pending));
        assert!(
            env.bucket_has_pack(&r.pack.name).await,
            "{} kept",
            r.pack.name
        );
    }
    let report = ok(prepared.publish(&env.node).await);
    assert_eq!(report.published.len(), outputs.len());
    env.assert_committed_files().await;
    let fresh = env.engine("node-fresh").await;
    assert_i1(&fresh, env.repo).await;
}

/// A push whose pack has the name of one the sweep is deleting (ADR 0004
/// item 5): pushed between the sweep's read and its claim, its pending row
/// stops the claim and the files stay; pushed after the claim, it is
/// refused retryably and its retry uploads the files again. Either way no
/// committed row is left without its files.
#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::too_many_lines)]
async fn sweep_claim_races_resent_push() {
    let env = env(12).await;
    let base = env.oid("refs/remotes/origin/main");
    let seed = env.committed_names().await;
    let mut packs = Vec::new();
    let mut tip = base;
    for i in 0..2 {
        let old = tip;
        tip = env.new_commit(old, &format!("race/{i}.txt"), format!("{i}\n").as_bytes());
        let pack = env.pack_for(tip).await;
        let before = env.committed_names().await;
        let res = push_support::raw_push(
            &env.node,
            env.repo,
            "pusher",
            &[(Some(old), Some(tip), "refs/heads/main")],
            Some(pack.clone()),
        )
        .await;
        assert!(res.ok("refs/heads/main"), "{res:?}");
        let name: Vec<String> = env
            .committed_names()
            .await
            .difference(&before)
            .cloned()
            .collect();
        assert_eq!(name.len(), 1);
        packs.push((tip, pack, name[0].clone()));
    }
    assert!(packs.iter().all(|(_, _, n)| !seed.contains(n)));
    let lease = env.lease("node-a").await;
    let report = ok(repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await);
    for (_, _, name) in &packs {
        assert!(
            report.superseded.iter().any(|p| &p.name == name),
            "{report:?}"
        );
    }

    // The resends run on another node, from inside the sweep.
    let other = env.engine("node-b").await;
    let repo = env.repo;
    let (tip_a, pack_a, name_a) = packs[0].clone();
    let (tip_b, pack_b, name_b) = packs[1].clone();
    let read_push = Arc::new(std::sync::Mutex::new(None));
    let claim_push = Arc::new(std::sync::Mutex::new(None));
    {
        let (other, out) = (Arc::clone(&other), Arc::clone(&read_push));
        env.node.set_fault_hook(
            FaultPoint::SweepAfterRead,
            Some(Arc::new(move |_| {
                let (other, out, pack) = (Arc::clone(&other), Arc::clone(&out), pack_a.clone());
                Box::pin(async move {
                    let res = push_support::raw_push(
                        &other,
                        repo,
                        "pusher",
                        &[(None, Some(tip_a), "refs/heads/again-a")],
                        Some(pack),
                    )
                    .await;
                    *out.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(res);
                })
            })),
        );
    }
    {
        let (other, out) = (Arc::clone(&other), Arc::clone(&claim_push));
        let pack_b = pack_b.clone();
        let name_b = name_b.clone();
        env.node.set_fault_hook(
            FaultPoint::SweepAfterClaim,
            Some(Arc::new(move |name| {
                let (other, out, pack) = (Arc::clone(&other), Arc::clone(&out), pack_b.clone());
                let mine = name == name_b;
                Box::pin(async move {
                    if !mine {
                        return;
                    }
                    let res = push_support::raw_push(
                        &other,
                        repo,
                        "pusher",
                        &[(None, Some(tip_b), "refs/heads/again-b")],
                        Some(pack),
                    )
                    .await;
                    *out.lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(res);
                })
            })),
        );
    }
    let swept = ok(sweep_packs(
        &env.node,
        env.repo,
        &lease,
        DEFAULT_GRACE,
        now() + 2 * GRACE_S,
        false,
    )
    .await);
    env.node.set_fault_hook(FaultPoint::SweepAfterRead, None);
    env.node.set_fault_hook(FaultPoint::SweepAfterClaim, None);
    let take = |m: &Arc<std::sync::Mutex<Option<push_support::RawResult>>>| {
        m.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| panic!("the hook pushed"))
    };

    // Between the read and the claim: the push commits and keeps the files.
    let a = take(&read_push);
    assert!(a.ok("refs/heads/again-a"), "{a:?}");
    assert!(
        swept
            .deleted
            .iter()
            .any(|d| d.name == name_a && d.state == PackState::Superseded),
        "{swept:?}"
    );
    assert!(env.bucket_has_pack(&name_a).await, "the files stay");
    env.assert_committed_files().await;

    // After the claim: refused retryably, and the files went.
    let b = take(&claim_push);
    let reason = b
        .ng("refs/heads/again-b")
        .unwrap_or_else(|| panic!("refused: {b:?}"));
    assert!(reason.contains("push again"), "{reason}");
    assert_eq!(env.db_ref("refs/heads/again-b").await, None);
    assert!(
        !env.bucket_has_pack(&name_b).await,
        "the claimed files went"
    );
    assert!(ok(env.world.index.pack_deletions(env.repo).await).is_empty());
    env.assert_committed_files().await;

    // The retry uploads them again.
    let retry = push_support::raw_push(
        &other,
        env.repo,
        "pusher",
        &[(None, Some(tip_b), "refs/heads/again-b")],
        Some(pack_b),
    )
    .await;
    assert!(retry.ok("refs/heads/again-b"), "{retry:?}");
    assert!(env.bucket_has_pack(&name_b).await);
    env.assert_committed_files().await;
    let fresh = env.engine("node-fresh").await;
    assert_i1(&fresh, env.repo).await;
}

/// A deletion claim a sweep that died left behind (ADR 0004 item 5) blocks
/// rows of its name until the next sweep finishes it: deleting the files if
/// no row names them, keeping them if one does.
#[tokio::test(flavor = "multi_thread")]
async fn sweep_finishes_an_interrupted_claim() {
    let (env, _) = pushed(12).await;
    let lease = env.lease("node-a").await;
    let committed = env.committed_names().await;
    let live = committed
        .iter()
        .next()
        .unwrap_or_else(|| panic!("a committed pack"))
        .clone();
    // Claims as a dead sweep would have left them: one on files nothing
    // names, and one on a committed pack, which only reindex can produce.
    let orphan = "pack-0123456789abcdef0123456789abcdef01234567";
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    let key = Key::Pack {
        repo: hex,
        name: ok(PackName::parse(orphan)),
        ext: PackExt::Pack,
    };
    ok(env
        .world
        .bucket
        .put(&key, bytes::Bytes::from_static(b"PACK"))
        .await);
    assert!(ok(env
        .world
        .index
        .claim_pack_deletion(env.repo, orphan)
        .await));
    ok(
        sqlx::query("INSERT INTO pack_deletions (repo, name, claimed_at) VALUES (?, ?, 0)")
            .bind(&env.repo.0[..])
            .bind(&live)
            .execute(&env.world.sql().await)
            .await,
    );

    let swept = ok(sweep_packs(&env.node, env.repo, &lease, DEFAULT_GRACE, now(), false).await);
    let finished: BTreeSet<&str> = swept.finished_claims.iter().map(String::as_str).collect();
    assert_eq!(finished, BTreeSet::from([orphan, live.as_str()]));
    assert!(
        !env.bucket_has_pack(orphan).await,
        "the orphan's files went"
    );
    assert!(env.bucket_has_pack(&live).await, "the named pack's stayed");
    assert!(ok(env.world.index.pack_deletions(env.repo).await).is_empty());
    env.assert_committed_files().await;
}

/// The leaseholder renews its lease every third of its TTL while a repack
/// runs (ADR 0004 item 5), so a repack that takes longer than the TTL still
/// publishes.
#[tokio::test(flavor = "multi_thread")]
async fn long_repack_renews_its_lease() {
    let (env, _) = pushed(12).await;
    let ttl = Duration::from_millis(600);
    let lease = ok(env.world.index.acquire_lease(env.repo, "node-a", ttl).await)
        .unwrap_or_else(|| panic!("the lease is free"));
    env.node.set_fault_hook(
        FaultPoint::RepackBeforeGit,
        Some(Arc::new(|_| {
            Box::pin(tokio::time::sleep(Duration::from_secs(2)))
        })),
    );
    let report = repack_full(&env.node, env.repo, &lease, DEFAULT_GRACE).await;
    env.node.set_fault_hook(FaultPoint::RepackBeforeGit, None);
    let report = ok(report);
    assert!(!report.published.is_empty(), "{report:?}");
    assert_eq!(
        ok(env.world.index.acquire_lease(env.repo, "node-b", ttl).await),
        None,
        "the holder still has it"
    );
}

/// Every maintenance pass finishes the deletion claims a sweep that died
/// left (ADR 0004 item 5), under the repack lease, so a byte-identical push
/// isn't refused until the next operator sweep; with the lease held
/// elsewhere it leaves them for that holder or the next pass.
#[tokio::test(flavor = "multi_thread")]
async fn maintenance_finishes_stale_claims() {
    let env = env(12).await;
    let orphan = "pack-89abcdef0123456789abcdef0123456789abcdef";
    let hex = ok(RepoIdHex::parse(&env.repo.to_string()));
    let key = Key::Pack {
        repo: hex,
        name: ok(PackName::parse(orphan)),
        ext: PackExt::Pack,
    };
    ok(env
        .world
        .bucket
        .put(&key, bytes::Bytes::from_static(b"PACK"))
        .await);
    // A claim a sweep that died two hours ago left.
    ok(sqlx::query(
        "INSERT INTO pack_deletions (repo, name, claimed_at) VALUES (?, ?, unixepoch() - 7200)",
    )
    .bind(&env.repo.0[..])
    .bind(orphan)
    .execute(&env.world.sql().await)
    .await);
    let row = |name: &str| openxet_index::Pack {
        id: openxet_index::PackId::generate(),
        repo: env.repo,
        name: name.to_owned(),
        state: PackState::Pending,
        created_at: now(),
        bytes: 4,
        midx_gen: None,
    };
    let blocked = env.world.index.insert_pack(&row(orphan)).await;
    assert!(
        matches!(blocked, Err(openxet_index::IndexError::PackDeleting { .. })),
        "{blocked:?}"
    );

    let held = env.lease("node-a").await;
    let report = ok(maintain(&env.node, "node-m").await);
    assert_eq!(report.claims_finished, 0, "the lease is held elsewhere");
    assert_eq!(ok(env.world.index.pack_deletions(env.repo).await).len(), 1);
    drop(held);
    ok(sqlx::query("UPDATE repo_leases SET expires_at = 0")
        .execute(&env.world.sql().await)
        .await);

    let report = ok(maintain(&env.node, "node-m").await);
    assert_eq!(report.claims_finished, 1, "{report:?}");
    assert!(ok(env.world.index.pack_deletions(env.repo).await).is_empty());
    assert!(!env.bucket_has_pack(orphan).await, "the claimed files went");
    ok(env.world.index.insert_pack(&row(orphan)).await);
}
