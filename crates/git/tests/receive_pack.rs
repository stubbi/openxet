//! receive-pack end to end (ADR 0004 item 3): real `git push` over smart
//! HTTP into [`Engine::receive_pack`], the hook relaying to the parent, the
//! pack published to the bucket and the refs moved by the fenced commit
//! transaction.

mod common;
mod push_support;

use std::sync::Arc;

use openxet_git::{
    FAULT_AFTER_COMMIT_ACTOR, FAULT_AFTER_UPLOAD_ACTOR, LARGE_BLOB_REPORT_BYTES, PackFiles,
    Publication, PublishError, Publisher,
};
use openxet_index::{CommitError, CommitTx, GitStore as _, Oid, PackState, RefUpdate};
use openxet_storage::{Bucket as _, Key, PackExt, PackName, RepoIdHex};

use common::{World, fixture_repo, ok, pack_objects};
use push_support::{
    Server, client_git, clone, commit, commit_tree, config, db_ref, engine, fetch, fsck, must, oid,
    pack_of, raw_push, seeded, text, work,
};

/// A push of new commits lands as a committed pack in the bucket with its
/// row, moves the refs, and a fresh node serves the result with a clean
/// `git fsck --strict`.
#[tokio::test(flavor = "multi_thread")]
async fn push_round_trip() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let node = engine(&world, config(&world, "node-a")).await;
    let server = Server::start(vec![node.clone()]).await;

    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    commit(&w, "one", &[("src/new.txt", b"new file\n")]);
    let tip = commit(&w, "two", &[("src/dir1/file1.txt", b"rewritten\n")]);
    must(
        &w,
        &["push", "-q", "origin", "main", "main:refs/heads/feature"],
    );

    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip));
    assert_eq!(db_ref(&world, repo, "refs/heads/feature").await, Some(tip));
    let snap = ok(world.index.snapshot(repo).await);
    assert_eq!(snap.packs.len(), 2, "the seed pack and the pushed pack");
    let pushed = snap
        .packs
        .iter()
        .find(|p| p.created_at != 1_761_400_000)
        .unwrap_or_else(|| panic!("the pushed pack is committed"));
    assert_eq!(pushed.state, PackState::Committed);
    let repo_hex = ok(RepoIdHex::parse(&repo.to_string()));
    for ext in [PackExt::Pack, PackExt::Idx] {
        let key = Key::Pack {
            repo: repo_hex,
            name: ok(PackName::parse(&pushed.name)),
            ext,
        };
        assert!(
            ok(world.bucket.get(&key).await).is_some(),
            "{key} is in the bucket"
        );
    }

    let fresh = engine(&world, config(&world, "node-b")).await;
    fsck(&fresh, repo).await;
    drop(server);
}

/// The Git cache is as private as the data directory it defaults into
/// (design §5.9): after a push, with a snapshot open on the hydrated packs,
/// every directory is `0700` and every file is its owner's alone, `0600`,
/// or `0400` for a cached pack file, which is read-only. The `proc-receive`
/// hook is a link to the openxet binary and holds no data.
#[tokio::test(flavor = "multi_thread")]
async fn git_cache_is_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let world = World::new().await;
    let (repo, fx) = seeded(&world, 5).await;
    let cache = config(&world, "node");
    let root = cache.cache_dir.clone();
    let node = engine(&world, cache).await;
    let server = Server::start(vec![node.clone()]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    commit(&w, "one", &[("a.txt", b"a\n")]);
    must(&w, &["push", "-q", "origin", "main"]);
    let snapshot = ok(node.snapshot(repo).await);

    let mut dirs = vec![root];
    let mut files = 0;
    while let Some(dir) = dirs.pop() {
        let mode = |p: &std::path::Path| ok(std::fs::metadata(p)).permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "{}", dir.display());
        for entry in ok(std::fs::read_dir(&dir)) {
            let path = ok(entry).path();
            let kind = ok(std::fs::symlink_metadata(&path)).file_type();
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                dirs.push(path);
            } else {
                assert!(
                    matches!(mode(&path), 0o600 | 0o400),
                    "{} is {:o}",
                    path.display(),
                    mode(&path)
                );
                files += 1;
            }
        }
    }
    assert!(files >= 6, "the lock, the snapshot's files and the packs");
    drop(snapshot);
    drop(server);
}

/// `ref_log_in_cas_transaction`, the push half (design §5.9 req 8): a push
/// writes one ref-log row per command, with the pusher and the request, in
/// its commit transaction; a refused push and a push stopped before its
/// commit write none.
#[tokio::test(flavor = "multi_thread")]
async fn ref_log_in_cas_transaction() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let node = engine(&world, config(&world, "node")).await;
    let server = Server::start(vec![node.clone()]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let before = db_ref(&world, repo, "refs/heads/main").await;
    let seq = ok(world.index.ref_log_since(repo, 0).await)
        .last()
        .map_or(0, |r| r.seq);

    let tip = commit(&w, "one", &[("a.txt", b"a\n")]);
    must(
        &w,
        &["push", "-q", "origin", "main", "main:refs/heads/feature"],
    );
    let log = ok(world.index.ref_log_since(repo, seq).await);
    assert_eq!(log.len(), 2, "one row per command: {log:?}");
    assert!(
        log.iter()
            .all(|r| r.actor == "pusher" && r.request == log[0].request && r.repo == repo)
    );
    let main = log
        .iter()
        .find(|r| r.name == "refs/heads/main")
        .unwrap_or_else(|| panic!("main is logged"));
    assert_eq!((main.old, main.new), (before, Some(tip)));
    let feature = log
        .iter()
        .find(|r| r.name == "refs/heads/feature")
        .unwrap_or_else(|| panic!("feature is logged"));
    assert_eq!((feature.old, feature.new), (None, Some(tip)));
    let seq = log.last().map_or(0, |r| r.seq);

    let run = client_git(&w, &["push", "origin", ":main"]);
    assert!(!run.success);
    commit(&w, "two", &[("b.txt", b"b\n")]);
    server.act_as(FAULT_AFTER_UPLOAD_ACTOR);
    assert!(!client_git(&w, &["push", "origin", "main"]).success);
    assert_eq!(ok(world.index.ref_log_since(repo, seq).await), Vec::new());
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip));
}

async fn pack_rows(world: &World, repo: openxet_index::RepoId) -> Vec<(String, String)> {
    let pool = world.sql().await;
    let rows: Vec<(String, String)> = ok(sqlx::query_as(
        "SELECT name, state FROM packs WHERE repo = ? ORDER BY created_at, name",
    )
    .bind(&repo.0[..])
    .fetch_all(&pool)
    .await);
    rows
}

async fn state_of(world: &World, name: &str) -> String {
    let pool = world.sql().await;
    let (state,): (String,) = ok(sqlx::query_as("SELECT state FROM packs WHERE name = ?")
        .bind(name)
        .fetch_one(&pool)
        .await);
    state
}

/// `failed_commit_marks_pack_lost` (ADR 0004): a publish whose commit
/// transaction fails on the ref CAS or on the repack fence rolls back as a
/// whole and leaves its pack `lost`; with a crash injected before that
/// update, the pack stays `pending` for GC.
#[tokio::test(flavor = "multi_thread")]
async fn failed_commit_marks_pack_lost() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let src = fixture_repo(&fx);
    let publisher = Publisher::new(world.index.clone(), world.bucket.clone());
    let main = db_ref(&world, repo, "refs/heads/main").await;
    let generation = ok(world.index.snapshot(repo).await).full_repack_gen;
    let out = world.dir.path().join("packs");
    let wrong: Oid = ok("12".repeat(20).parse());

    let cases: [(&str, Option<Oid>, u64, &str); 3] = [
        ("cas", Some(wrong), generation, "tester"),
        ("fence", main, generation + 1, "tester"),
        ("crash", main, generation, FAULT_AFTER_UPLOAD_ACTOR),
    ];
    for (i, (case, old, gen_, actor)) in cases.into_iter().enumerate() {
        let files = pack_objects(
            &src,
            &format!("main~{}\n^main~{}", i + 1, i + 2),
            &out.join(case),
        );
        let pack = PackFiles {
            dir: files.dir.clone(),
            name: ok(PackName::parse(&files.name)),
        };
        let updates = [RefUpdate {
            name: format!("refs/heads/{case}"),
            old: None,
            new: Some(ok(must(&src, &["rev-parse", &format!("main~{}", i + 1)])
                .trim()
                .parse())),
        }];
        let updates = if case == "cas" {
            vec![RefUpdate {
                name: "refs/heads/main".to_owned(),
                old,
                new: updates[0].new,
            }]
        } else {
            updates.to_vec()
        };
        let result = publisher
            .publish(Publication {
                repo,
                pack: Some(&pack),
                updates: &updates,
                full_repack_gen: gen_,
                actor,
                request: "test",
            })
            .await;
        let expected = match case {
            "cas" => {
                assert!(
                    matches!(
                        result,
                        Err(PublishError::Commit(CommitError::RefMismatch(_)))
                    ),
                    "{result:?}"
                );
                "lost"
            }
            "fence" => {
                assert!(
                    matches!(result, Err(PublishError::Commit(CommitError::Fenced))),
                    "{result:?}"
                );
                "lost"
            }
            _ => {
                assert!(
                    matches!(result, Err(PublishError::Injected(_))),
                    "{result:?}"
                );
                "pending"
            }
        };
        assert_eq!(state_of(&world, &files.name).await, expected, "{case}");
        assert_eq!(
            db_ref(&world, repo, "refs/heads/main").await,
            main,
            "{case}"
        );
        assert_eq!(
            db_ref(&world, repo, &format!("refs/heads/{case}")).await,
            None,
            "{case}"
        );
    }
    assert!(
        ok(world.index.ref_log_since(repo, 0).await)
            .iter()
            .all(|r| r.actor != FAULT_AFTER_UPLOAD_ACTOR),
        "a failed transaction writes no ref-log row"
    );
}

/// `cas_requires_pending_pack` and the push cases of
/// `publish_then_cas_survives_faults` (ADR 0004): a node killed between
/// publish and commit leaves the ref alone and the pack `pending`; once GC
/// expires it, a late commit of it fails cleanly, and the push succeeds when
/// retried. A node killed between commit and answer leaves a moved ref whose
/// objects are committed, so no ref dangles, and the retry finds it done.
#[tokio::test(flavor = "multi_thread")]
async fn publish_then_cas_survives_faults() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let node = engine(&world, config(&world, "node")).await;
    let server = Server::start(vec![node.clone()]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let main = db_ref(&world, repo, "refs/heads/main").await;

    // Killed between publish and commit.
    let tip = commit(&w, "one", &[("a.txt", b"a\n")]);
    server.act_as(FAULT_AFTER_UPLOAD_ACTOR);
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(!run.success, "an unanswered push is not acknowledged");
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, main);
    let rows = pack_rows(&world, repo).await;
    let (pending, state) = rows.last().cloned().unwrap_or_default();
    assert_eq!(state, "pending");
    let key = Key::Pack {
        repo: ok(RepoIdHex::parse(&repo.to_string())),
        name: ok(PackName::parse(&pending)),
        ext: PackExt::Pack,
    };
    assert!(
        ok(world.bucket.get(&key).await).is_some(),
        "the pack was published first"
    );
    let id = ok(world.index.snapshot(repo).await);
    assert!(
        id.packs.iter().all(|p| p.name != pending),
        "a pending pack is never in a snapshot"
    );

    // GC expires it; a late commit can't use it.
    let pool = world.sql().await;
    let (pack_id,): (String,) = ok(sqlx::query_as("SELECT hex(id) FROM packs WHERE name = ?")
        .bind(&pending)
        .fetch_one(&pool)
        .await);
    let pack_id = ok(pack_id.parse());
    assert!(ok(world
        .index
        .mark_pack(pack_id, PackState::Pending, PackState::Expired)
        .await));
    let late = world
        .index
        .commit(CommitTx {
            repo,
            updates: &[RefUpdate {
                name: "refs/heads/main".to_owned(),
                old: main,
                new: Some(tip),
            }],
            pack: Some(pack_id),
            full_repack_gen: ok(world.index.snapshot(repo).await).full_repack_gen,
            actor: "late",
            request: "late",
        })
        .await;
    assert!(matches!(late, Err(CommitError::PackNotPending)), "{late:?}");
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, main);

    server.act_as("pusher");
    must(&w, &["push", "-q", "origin", "main"]);
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip));

    // Killed between commit and answer.
    let tip2 = commit(&w, "two", &[("b.txt", b"b\n")]);
    server.act_as(FAULT_AFTER_COMMIT_ACTOR);
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(!run.success, "the answer never came");
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip2));
    let rows = pack_rows(&world, repo).await;
    let committed = rows.iter().filter(|(_, s)| s == "committed").count();
    assert_eq!(
        committed, 3,
        "the seed, the retried push and this one: {rows:?}"
    );
    server.act_as("pusher");
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(
        run.success && run.stderr.contains("Everything up-to-date"),
        "{}",
        run.stderr
    );

    let fresh = engine(&world, config(&world, "fresh")).await;
    fsck(&fresh, repo).await;
}

/// `push_cannot_use_foreign_pending_objects` (ADR 0004): a push whose
/// commit names an object that exists only in another writer's `pending`
/// or `lost` pack is refused by receive-pack's own checks, which see only
/// the snapshot's committed packs.
#[tokio::test(flavor = "multi_thread")]
async fn push_cannot_use_foreign_pending_objects() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let node = engine(&world, config(&world, "node")).await;
    let server = Server::start(vec![node.clone()]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let main = oid(&w, "HEAD");

    for state in [PackState::Pending, PackState::Lost] {
        let foreign = commit(
            &w,
            &format!("foreign {state:?}"),
            &[("f.txt", &text(3, 50))],
        );
        let files = pack_objects(
            &w,
            &format!("{foreign}\n^{main}"),
            &world.dir.path().join(format!("{state:?}")),
        );
        let row = world.pending(repo, &files, 1_761_400_100).await;
        if state == PackState::Lost {
            assert!(ok(world
                .index
                .mark_pack(row.id, PackState::Pending, PackState::Lost)
                .await));
        }
        let child = commit_tree(&w, "HEAD^{tree}", &[foreign], &[]);
        let r = raw_push(
            &node,
            repo,
            "pusher",
            &[(None, Some(child), "refs/heads/x")],
            Some(pack_of(&w, &[child])),
        )
        .await;
        assert!(!r.ok("refs/heads/x"), "{state:?}: {r:?}");
        assert!(r.ng("refs/heads/x").is_some(), "{state:?}: {r:?}");
        assert_eq!(db_ref(&world, repo, "refs/heads/x").await, None);
        must(&w, &["reset", "-q", "--hard", &main.to_string()]);
    }
    drop(server);
}

/// `concurrent_push_one_winner` (ADR 0004, from S9): 8 clients on 2 nodes
/// sharing one index and bucket push to `main` at once, 20 rounds. Each
/// round has exactly one winner, the index's tip is the winner's commit,
/// every loser's pack is `lost` or never written, and a fresh node's
/// `fsck --strict` is clean.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_push_one_winner() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let nodes = vec![
        engine(&world, config(&world, "node-a")).await,
        engine(&world, config(&world, "node-b")).await,
    ];
    let server = Arc::new(Server::start(nodes.clone()).await);
    let clients: Vec<_> = (0..8)
        .map(|i| clone(&fixture_repo(&fx), &server.url(i % 2, repo), &[]))
        .collect();

    for round in 0..20 {
        let mut tips = Vec::new();
        for (i, client) in clients.iter().enumerate() {
            let w = work(client);
            fetch(&nodes[i % 2], repo, &w).await;
            must(&w, &["reset", "-q", "--hard", "origin/main"]);
            tips.push(commit(
                &w,
                &format!("round {round} client {i}"),
                &[(&format!("c{i}.txt"), format!("{round}\n").as_bytes())],
            ));
        }
        let pushes: Vec<_> = clients
            .iter()
            .map(|c| {
                let w = work(c);
                tokio::task::spawn_blocking(move || client_git(&w, &["push", "origin", "main"]))
            })
            .collect();
        let mut winners = Vec::new();
        for (i, push) in pushes.into_iter().enumerate() {
            if ok(push.await).success {
                winners.push(i);
            }
        }
        assert_eq!(
            winners.len(),
            1,
            "round {round}: one winner, got {winners:?}"
        );
        assert_eq!(
            db_ref(&world, repo, "refs/heads/main").await,
            Some(tips[winners[0]]),
            "round {round}"
        );
    }
    let rows = pack_rows(&world, repo).await;
    assert_eq!(
        rows.iter().filter(|(_, s)| s == "committed").count(),
        21,
        "the seed and 20 winners"
    );
    assert!(
        rows.iter().all(|(_, s)| s == "committed" || s == "lost"),
        "{rows:?}"
    );
    let fresh = engine(&world, config(&world, "fresh")).await;
    fsck(&fresh, repo).await;
}

/// Review Focus 3: a large blob that isn't an LFS pointer is accepted and
/// kept as a Git object, and the push output names the file and its size
/// and suggests `git lfs track`.
#[tokio::test(flavor = "multi_thread")]
async fn plain_large_blob_reported() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let node = engine(&world, config(&world, "node")).await;
    let server = Server::start(vec![node]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let big = text(5, 20 * 1024 * 1024 / 48);
    assert!(big.len() as u64 >= LARGE_BLOB_REPORT_BYTES);
    let tip = commit(&w, "weights", &[("weights/model.bin", &big)]);
    let small = commit(&w, "small", &[("notes.txt", b"small\n")]);
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(run.success, "{}", run.stderr);
    let tenths = (big.len() * 10 + 524_288) / 1_048_576;
    let size = format!("{}.{} MiB", tenths / 10, tenths % 10);
    assert!(
        run.stderr.contains(&format!("remote: openxet: weights/model.bin is {size} and stored as a plain Git object, not with Git LFS"))
            && run.stderr.contains("git lfs track \"weights/model.bin\""),
        "{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("notes.txt"),
        "small files aren't named"
    );
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(small));
    let _ = tip;
}

/// Review Focus 3: above `git.max_plain_blob_bytes` the same push is
/// refused, with the same message.
#[tokio::test(flavor = "multi_thread")]
async fn plain_large_blob_refused_above_limit() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let mut cfg = config(&world, "node");
    cfg.max_plain_blob_bytes = Some(16 * 1024 * 1024);
    let node = engine(&world, cfg).await;
    let server = Server::start(vec![node]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let main = db_ref(&world, repo, "refs/heads/main").await;
    let big = text(5, 20 * 1024 * 1024 / 48);
    commit(&w, "weights", &[("weights/model.bin", &big)]);
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(!run.success);
    assert!(
        run.stderr.contains("weights/model.bin is")
            && run.stderr.contains("git lfs track \"weights/model.bin\"")
            && run
                .stderr
                .contains("This server refuses files over 16.0 MiB outside Git LFS."),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("[remote rejected]"), "{}", run.stderr);
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, main);
    let rows = pack_rows(&world, repo).await;
    assert_eq!(rows.len(), 1, "nothing published: {rows:?}");
}

/// Review Focus 3: however low `git.max_plain_blob_bytes` is set, it never
/// refuses an LFS pointer, which is under 1024 bytes (git-lfs spec).
#[tokio::test(flavor = "multi_thread")]
async fn plain_blob_limit_never_refuses_lfs_pointers() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let mut cfg = config(&world, "node");
    cfg.max_plain_blob_bytes = Some(10);
    let node = engine(&world, cfg).await;
    let server = Server::start(vec![node]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let pointer = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize 123456789\n",
        "4d".repeat(32)
    );
    let tip = commit(&w, "pointer", &[("model.safetensors", pointer.as_bytes())]);
    must(&w, &["push", "-q", "origin", "main"]);
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip));

    commit(&w, "plain", &[("plain.bin", &text(9, 50))]);
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(!run.success);
    assert!(run.stderr.contains("plain.bin is"), "{}", run.stderr);
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip));
}

/// A push whose answer was lost, sent again: its pack is named like the
/// committed one (a pack is named by its checksum), so nothing new is
/// published, and its ref update still commits.
#[tokio::test(flavor = "multi_thread")]
async fn resent_pack_publishes_nothing_new() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let node = engine(&world, config(&world, "node")).await;
    let client = clone(&fixture_repo(&fx), "unused", &[]);
    let w = work(&client);
    let main = db_ref(&world, repo, "refs/heads/main").await;
    let tip = commit(&w, "one", &[("a.txt", b"a\n")]);
    let pack = push_support::pack_revs(&w, &format!("{tip}\n^{}", main.unwrap_or(tip)), false);

    let first = raw_push(
        &node,
        repo,
        "pusher",
        &[(main, Some(tip), "refs/heads/main")],
        Some(pack.clone()),
    )
    .await;
    assert!(first.ok("refs/heads/main"), "{first:?}");
    let rows = pack_rows(&world, repo).await;
    assert_eq!(rows.len(), 2, "the seed and the push: {rows:?}");

    let again = raw_push(
        &node,
        repo,
        "pusher",
        &[(None, Some(tip), "refs/heads/again")],
        Some(pack),
    )
    .await;
    assert!(again.ok("refs/heads/again"), "{again:?}");
    assert_eq!(db_ref(&world, repo, "refs/heads/again").await, Some(tip));
    assert_eq!(pack_rows(&world, repo).await, rows, "no pack row added");
    fsck(&node, repo).await;
}

/// A push turned away by R22's concurrency limits was never checked, so
/// the writer rule's counters don't count it as a refusal.
#[tokio::test(flavor = "multi_thread")]
async fn busy_push_is_not_counted_as_refused() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let mut cfg = config(&world, "node");
    cfg.reachability.max_concurrent_per_repo = Some(0);
    let node = engine(&world, cfg).await;
    let src = fixture_repo(&fx);
    let main = db_ref(&world, repo, "refs/heads/main").await;
    let away = common::rev(&src, "main~1");
    // `main~1` stays in the committed pack, but no ref reaches it once
    // `main` is moved to `main~2` and the tag of the tip is deleted.
    let generation = ok(world.index.snapshot(repo).await).full_repack_gen;
    ok(world
        .index
        .commit(CommitTx {
            repo,
            updates: &[
                common::set("refs/heads/main", main, common::rev(&src, "main~2")),
                RefUpdate {
                    name: "refs/tags/v1.0".to_owned(),
                    old: Some(common::rev(&src, "refs/tags/v1.0")),
                    new: None,
                },
            ],
            pack: None,
            full_repack_gen: generation,
            actor: "tester",
            request: "test",
        })
        .await);
    let empty = pack_of(&src, &[]);
    let before = node.writer_rule_counts();
    let r = raw_push(
        &node,
        repo,
        "pusher",
        &[(None, Some(away), "refs/heads/x")],
        Some(empty),
    )
    .await;
    let reason = r.ng("refs/heads/x").unwrap_or_default();
    assert!(reason.contains("too many pushes"), "{r:?}");
    assert_eq!(node.writer_rule_counts(), before, "{r:?}");
}
