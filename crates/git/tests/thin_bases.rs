//! The writer reachability rule (ADR 0004 item 3, design §5.9 req 8): a
//! push can't make an object reachable that sits in a committed pack but
//! that no ref reaches, by any route, and honest thin pushes still land.

mod common;
mod push_support;

use std::path::Path;

use openxet_git::{Engine, MISSING_OBJECTS, THIN_HINT, WriterRuleCounts};
use openxet_index::{Oid, RepoId};

use common::{World, fixture_repo};
use push_support::{
    Server, blob, clone, commit, commit_tree, config, db_ref, engine, fetch, must, oid, pack_of,
    pack_revs, pack_thin, raw_push, seeded, text, work,
};

/// The repository with an unreachable commit `doomed` (adding
/// `secret.txt`) in a committed pack: pushed on a branch, then the branch
/// deleted.
struct Setup {
    world: World,
    repo: RepoId,
    node: std::sync::Arc<Engine>,
    server: Server,
    client: tempfile::TempDir,
    main: Oid,
    doomed: Oid,
    secret: Oid,
    _fixture: tempfile::TempDir,
}

async fn setup() -> Setup {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let node = engine(&world, config(&world, "node")).await;
    let server = Server::start(vec![node.clone()]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let main = oid(&w, "HEAD");
    must(&w, &["checkout", "-q", "-b", "doomed"]);
    let doomed = commit(&w, "doomed", &[("secret.txt", &text(7, 200))]);
    let secret = oid(&w, "HEAD:secret.txt");
    must(&w, &["push", "-q", "origin", "doomed"]);
    must(&w, &["push", "-q", "origin", ":doomed"]);
    must(&w, &["checkout", "-q", "main"]);
    assert_eq!(db_ref(&world, repo, "refs/heads/doomed").await, None);
    Setup {
        world,
        repo,
        node,
        server,
        client,
        main,
        doomed,
        secret,
        _fixture: fx,
    }
}

fn delta(before: WriterRuleCounts, after: WriterRuleCounts) -> [u64; 4] {
    [
        after.sent - before.sent,
        after.fast_path - before.fast_path,
        after.walk - before.walk,
        after.refused - before.refused,
    ]
}

const SENT: [u64; 4] = [1, 0, 0, 0];
const FAST: [u64; 4] = [0, 1, 0, 0];
const WALK: [u64; 4] = [0, 0, 1, 0];
const REFUSED: [u64; 4] = [0, 0, 0, 1];

/// The four routes of ADR 0004 (`writer_cannot_make_unreachable_reachable`):
/// a ref at the object, the object as a parent or tree entry, a `REF_DELTA`
/// against it with a tree naming it, and a delta copying its bytes. Each is
/// refused with git's own missing-object text, and the same push succeeds
/// when it sends the object.
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: four routes on one unreachable commit"
)]
async fn writer_cannot_make_unreachable_reachable() {
    let s = setup().await;
    let w = work(&s.client);
    let node = &s.node;
    let empty = pack_of(&w, &[]);

    // git's own text for an object that doesn't exist at all.
    let absent: Oid = "ab".repeat(20).parse().unwrap_or_else(|e| panic!("{e}"));
    let git_text = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(absent), "refs/heads/y")],
        Some(empty.clone()),
    )
    .await
    .ng("refs/heads/y")
    .unwrap_or_else(|| panic!("git refuses an absent object"));
    assert_eq!(git_text, MISSING_OBJECTS);

    // Route 1: a ref pointing at the unreachable commit.
    let before = node.writer_rule_counts();
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(s.doomed), "refs/heads/x")],
        Some(empty.clone()),
    )
    .await;
    assert_eq!(r.ng("refs/heads/x"), Some(git_text.clone()), "{r:?}");
    assert!(!r.messages.contains(THIN_HINT));
    assert_eq!(delta(before, node.writer_rule_counts()), REFUSED);

    // Route 2: the unreachable commit as a parent ...
    let child = commit_tree(&w, "main^{tree}", &[s.doomed], &[]);
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(child), "refs/heads/x")],
        Some(pack_of(&w, &[child])),
    )
    .await;
    assert_eq!(r.ng("refs/heads/x"), Some(git_text.clone()), "{r:?}");
    // ... and its blob as a tree entry.
    let named = commit_tree(&w, "main^{tree}", &[s.main], &[("copy.txt", s.secret)]);
    let named_tree = oid(&w, &format!("{named}^{{tree}}"));
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(named), "refs/heads/z")],
        Some(pack_of(&w, &[named, named_tree])),
    )
    .await;
    assert_eq!(r.ng("refs/heads/z"), Some(git_text.clone()), "{r:?}");

    // Route 3: a REF_DELTA against the unreachable blob, with a tree naming
    // it, so that `--fix-thin` appends it as a base.
    let mut similar = text(7, 200);
    similar.extend_from_slice(b"one more line\n");
    let d = blob(&w, &similar);
    let thin3 = commit_tree(
        &w,
        "main^{tree}",
        &[s.main],
        &[("copy.txt", s.secret), ("secret.txt", d)],
    );
    let before = node.writer_rule_counts();
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(thin3), "refs/heads/t3")],
        Some(pack_thin(
            &w,
            s.doomed,
            &[
                (thin3, ""),
                (oid(&w, &format!("{thin3}^{{tree}}")), ""),
                (d, "secret.txt"),
            ],
        )),
    )
    .await;
    assert_eq!(r.ng("refs/heads/t3"), Some(git_text.clone()), "{r:?}");
    assert!(r.messages.contains(THIN_HINT), "{r:?}");
    assert_eq!(delta(before, node.writer_rule_counts()), REFUSED);
    // Route 4: a delta that copies the unreachable blob's bytes, the blob
    // named nowhere.
    let mut copied = text(7, 200);
    copied.extend_from_slice(b"another line\n");
    let d4 = blob(&w, &copied);
    let thin4 = commit_tree(&w, "main^{tree}", &[s.main], &[("secret.txt", d4)]);
    let before = node.writer_rule_counts();
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(thin4), "refs/heads/t4")],
        Some(pack_thin(
            &w,
            s.doomed,
            &[
                (thin4, ""),
                (oid(&w, &format!("{thin4}^{{tree}}")), ""),
                (d4, "secret.txt"),
            ],
        )),
    )
    .await;
    assert_eq!(r.ng("refs/heads/t4"), Some(git_text.clone()), "{r:?}");
    assert!(r.messages.contains(THIN_HINT), "{r:?}");
    assert_eq!(delta(before, node.writer_rule_counts()), REFUSED);
    // The same pushes succeed when they send the objects themselves.
    let doomed_tree = oid(&w, &format!("{}^{{tree}}", s.doomed));
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(child), "refs/heads/x")],
        Some(pack_of(&w, &[child, s.doomed, doomed_tree, s.secret])),
    )
    .await;
    assert!(
        r.ok("refs/heads/x"),
        "sending the object itself succeeds: {r:?}"
    );

    let thin3_tree = oid(&w, &format!("{thin3}^{{tree}}"));
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(thin3), "refs/heads/t3")],
        Some(pack_of(&w, &[thin3, thin3_tree, d, s.secret])),
    )
    .await;
    assert!(
        r.ok("refs/heads/t3"),
        "sending the base itself succeeds: {r:?}"
    );

    let before = node.writer_rule_counts();
    let r = raw_push(
        node,
        s.repo,
        "pusher",
        &[(None, Some(thin4), "refs/heads/t4")],
        Some(pack_revs(&w, &format!("{thin4}\n^{}", s.main), false)),
    )
    .await;
    assert!(
        r.ok("refs/heads/t4"),
        "the same push without thin bases succeeds: {r:?}"
    );
    assert_eq!(delta(before, node.writer_rule_counts()), SENT);

    for name in ["refs/heads/x", "refs/heads/t3", "refs/heads/t4"] {
        assert!(db_ref(&s.world, s.repo, name).await.is_some(), "{name}");
    }
    assert_eq!(db_ref(&s.world, s.repo, "refs/heads/z").await, None);
    assert_eq!(db_ref(&s.world, s.repo, "refs/heads/y").await, None);
}

/// A stock thin push from an up-to-date full clone takes its bases from the
/// boundary commit's tree at the same path: the fast path.
#[tokio::test(flavor = "multi_thread")]
async fn stock_thin_push_takes_the_fast_path() {
    let s = setup().await;
    let w = work(&s.client);
    let mut contents =
        std::fs::read(w.join("src/dir1/file1.txt")).unwrap_or_else(|e| panic!("{e}"));
    contents.extend_from_slice(b"value = 1\n");
    commit(&w, "edit", &[("src/dir1/file1.txt", &contents)]);
    let before = s.node.writer_rule_counts();
    must(&w, &["push", "-q", "origin", "main"]);
    assert_eq!(delta(before, s.node.writer_rule_counts()), FAST);
}

/// Pushes `refspecs` from `dir`; if refused with the thin-pack hint, pushes
/// again as the hint says, which must succeed. Returns the paths the
/// rule took, in order.
fn push_or_retry(node: &Engine, dir: &Path, refspecs: &[&str]) -> Vec<[u64; 4]> {
    let mut args = vec!["push", "origin"];
    args.extend_from_slice(refspecs);
    let before = node.writer_rule_counts();
    let first = push_support::client_git(dir, &args);
    let mut paths = vec![delta(before, node.writer_rule_counts())];
    if !first.success {
        assert!(
            first.stderr.contains(THIN_HINT),
            "refused without the hint: {}",
            first.stderr
        );
        assert!(first.stderr.contains(MISSING_OBJECTS), "{}", first.stderr);
        let mut args = vec!["-c", "pack.window=0", "push", "origin"];
        args.extend_from_slice(refspecs);
        let before = node.writer_rule_counts();
        let second = push_support::client_git(dir, &args);
        assert!(
            second.success,
            "the hinted push must succeed: {}",
            second.stderr
        );
        paths.push(delta(before, node.writer_rule_counts()));
    }
    paths
}

/// The known-limitation clients of ADR 0004 item 3: a bare client with
/// bitmaps, a shallow client pushing two refs, and a client whose view of
/// the server's refs is older than the snapshot. Each either succeeds or is
/// refused with the `--no-thin` hint and then succeeds with `--no-thin`;
/// the path each takes is pinned, so a fast-path regression fails here.
#[tokio::test(flavor = "multi_thread")]
async fn known_limitation_clients() {
    let s = setup().await;
    let url = s.server.url(0, s.repo);

    // A bare client with a bitmap of everything it believes the server has.
    let snap = s
        .node
        .snapshot(s.repo)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let bare = clone(snap.dir.path(), &url, &["--bare"]);
    drop(snap);
    let bare_dir = bare.path().join("work");
    must(&bare_dir, &["repack", "-adb", "-q"]);
    let staging = clone(&bare_dir, &bare_dir.to_string_lossy(), &[]);
    let sw = work(&staging);
    let mut contents =
        std::fs::read(sw.join("src/dir2/file2.txt")).unwrap_or_else(|e| panic!("{e}"));
    contents.extend_from_slice(b"value = 2\n");
    commit(&sw, "bitmap client", &[("src/dir2/file2.txt", &contents)]);
    must(&sw, &["push", "-q", "origin", "main"]);
    assert_eq!(
        push_or_retry(&s.node, &bare_dir, &["main"]),
        vec![SENT],
        "bitmap client"
    );

    // A shallow client with two refs, pushing both.
    let snap = s
        .node
        .snapshot(s.repo)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let source = format!("file://{}", snap.dir.path().display());
    let shallow = clone(Path::new(&source), &url, &["--depth", "1"]);
    drop(snap);
    let hw = work(&shallow);
    must(&hw, &["checkout", "-q", "-b", "second"]);
    let mut contents =
        std::fs::read(hw.join("src/dir3/file3.txt")).unwrap_or_else(|e| panic!("{e}"));
    contents.extend_from_slice(b"value = 3\n");
    commit(&hw, "shallow second", &[("src/dir3/file3.txt", &contents)]);
    must(&hw, &["checkout", "-q", "main"]);
    let mut contents =
        std::fs::read(hw.join("src/dir4/file4.txt")).unwrap_or_else(|e| panic!("{e}"));
    contents.extend_from_slice(b"value = 4\n");
    commit(&hw, "shallow main", &[("src/dir4/file4.txt", &contents)]);
    assert_eq!(
        push_or_retry(&s.node, &hw, &["main", "second"]),
        vec![FAST],
        "shallow client"
    );

    // A stale client: it builds on `topic`, which another client deletes
    // after the stale client last saw the refs. Its thin bases sit in a
    // commit the snapshot no longer reaches: refused with the hint, then
    // the hinted push from a fresh view sends the whole closure.
    let sc = work(&s.client);
    fetch(&s.node, s.repo, &sc).await;
    must(&sc, &["reset", "-q", "--hard", "origin/main"]);
    must(&sc, &["checkout", "-q", "-b", "topic"]);
    let mut contents =
        std::fs::read(sc.join("src/dir5/file5.txt")).unwrap_or_else(|e| panic!("{e}"));
    contents.extend_from_slice(&text(11, 100));
    commit(&sc, "topic", &[("src/dir5/file5.txt", &contents)]);
    must(&sc, &["push", "-q", "origin", "topic"]);
    s.server.freeze_advertisement(0, s.repo).await;
    let other = clone(&sc, &url, &[]);
    must(&work(&other), &["push", "-q", "origin", ":topic"]);
    contents.extend_from_slice(b"value = 5\n");
    commit(&sc, "stale", &[("src/dir5/file5.txt", &contents)]);
    let before = s.node.writer_rule_counts();
    let first = push_support::client_git(&sc, &["push", "origin", "topic:refs/heads/stale"]);
    assert!(
        !first.success,
        "builds on a deleted branch: {}",
        first.stderr
    );
    assert!(first.stderr.contains(THIN_HINT), "{}", first.stderr);
    assert_eq!(delta(before, s.node.writer_rule_counts()), REFUSED);
    s.server.thaw_advertisement();
    let before = s.node.writer_rule_counts();
    must(
        &sc,
        &[
            "-c",
            "pack.window=0",
            "push",
            "-q",
            "origin",
            "topic:refs/heads/stale",
        ],
    );
    assert_eq!(
        delta(before, s.node.writer_rule_counts()),
        SENT,
        "stale client"
    );
}

/// A thin base that is reachable but sits in no boundary or tip tree at the
/// path the push uses it is placed by the fallback walk, within
/// `git.thin_bases`; past the object or time budget the push is refused
/// with the hint (fail closed).
#[tokio::test(flavor = "multi_thread")]
async fn fallback_walk_places_deep_bases_within_budget() {
    let s = setup().await;
    let w = work(&s.client);
    let deep = oid(&w, "main~10");
    let path = "src/dir0/file0.txt";
    let base = oid(&w, &format!("main~10:{path}"));
    let mut contents = must(&w, &["cat-file", "blob", &base.to_string()]).into_bytes();
    contents.extend_from_slice(b"value = 0\n");
    let d = blob(&w, &contents);
    let moved = commit_tree(&w, "main^{tree}", &[s.main], &[("moved.txt", d)]);
    let pack = || {
        pack_thin(
            &w,
            deep,
            &[
                (moved, ""),
                (oid(&w, &format!("{moved}^{{tree}}")), ""),
                (d, path),
            ],
        )
    };

    for (name, budget) in [
        (
            "objects",
            openxet_git::Budget {
                max_objects: 5,
                ..openxet_git::Budget::THIN_BASES
            },
        ),
        (
            "time",
            openxet_git::Budget {
                max_time: std::time::Duration::ZERO,
                ..openxet_git::Budget::THIN_BASES
            },
        ),
    ] {
        let mut cfg = config(&s.world, &format!("tight-{name}"));
        cfg.thin_bases = budget;
        let tight = engine(&s.world, cfg).await;
        let before = tight.writer_rule_counts();
        let r = raw_push(
            &tight,
            s.repo,
            "pusher",
            &[(None, Some(moved), "refs/heads/moved")],
            Some(pack()),
        )
        .await;
        assert_eq!(
            r.ng("refs/heads/moved").as_deref(),
            Some(MISSING_OBJECTS),
            "{name}: {r:?}"
        );
        assert!(r.messages.contains(THIN_HINT), "{name}: {r:?}");
        assert_eq!(delta(before, tight.writer_rule_counts()), REFUSED, "{name}");
    }

    // The walk shares R22's per-repository and per-principal limits: with
    // no slot free it is refused as retryable, before any walking, while a
    // push that needs no walk is unaffected.
    for (name, repo_max, principal_max) in [("repo", Some(0), None), ("principal", None, Some(0))] {
        let mut cfg = config(&s.world, &format!("busy-{name}"));
        cfg.reachability.max_concurrent_per_repo = repo_max;
        cfg.reachability.max_concurrent_per_principal = principal_max;
        let busy = engine(&s.world, cfg).await;
        let r = raw_push(
            &busy,
            s.repo,
            "pusher",
            &[(None, Some(moved), "refs/heads/moved")],
            Some(pack()),
        )
        .await;
        let reason = r.ng("refs/heads/moved").unwrap_or_default();
        assert!(
            reason.contains("push again in a few seconds"),
            "{name}: {r:?}"
        );
        assert!(!r.messages.contains(THIN_HINT), "{name}: {r:?}");
        let sent_all = commit_tree(&w, "main^{tree}", &[s.main], &[("sent.txt", d)]);
        let sent_tree = oid(&w, &format!("{sent_all}^{{tree}}"));
        let r = raw_push(
            &busy,
            s.repo,
            "pusher",
            &[(None, Some(sent_all), &format!("refs/heads/sent-{name}"))],
            Some(pack_of(&w, &[sent_all, sent_tree, d])),
        )
        .await;
        assert!(r.ok(&format!("refs/heads/sent-{name}")), "{name}: {r:?}");
    }

    let before = s.node.writer_rule_counts();
    let r = raw_push(
        &s.node,
        s.repo,
        "pusher",
        &[(None, Some(moved), "refs/heads/moved")],
        Some(pack()),
    )
    .await;
    assert!(r.ok("refs/heads/moved"), "{r:?}");
    assert_eq!(delta(before, s.node.writer_rule_counts()), WALK);
}
