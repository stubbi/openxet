//! The Hub object layer (ADR 0004 item 6): Hub reads at a revision and Hub
//! writes, all on one request snapshot. Reads by object id pass R22; Hub
//! commits and annotated tags are packs built in-process, indexed with
//! `git index-pack --strict`, checked for connectivity and by the writer
//! reachability rule, and published like a push; every Hub ref write runs
//! the fenced commit transaction.

mod common;
mod push_support;

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use openxet_git::{
    Blob, Budget, Engine, EntryKind, FAULT_OMIT_BLOBS_ACTOR, GitError, HubCommit, Ident,
    MISSING_OBJECTS, Op, Pointer, Revision, TagInfo, canonical_pointer, git_command, parse_pointer,
};
use openxet_index::{CommitTx, GitStore as _, Oid, RefLogRow, RefUpdate, RepackPublish, RepoId};
use openxet_storage::{Bucket as _, Key, PackExt, PackName, RepoIdHex};

use common::{PackFiles, World, fixture_repo, git, git_with_input, ok, pack_objects, rev, set};
use push_support::{blob, commit_tree, config, db_ref, engine, fsck, principal, seeded};

const MAIN: &str = "refs/heads/main";

/// The subject Hub reads run as.
const READER: &str = "reader";

/// What no ref reaches any more in the tests' unreachable commit.
const SECRET: &[u8] = b"no ref reaches this any more\n";

async fn node(world: &World, name: &str) -> Arc<Engine> {
    engine(world, config(world, name)).await
}

fn file(path: &str, content: &[u8]) -> Op {
    Op::File {
        path: path.to_owned(),
        content: Bytes::copy_from_slice(content),
    }
}

fn upload(ops: Vec<Op>) -> HubCommit<'static> {
    HubCommit {
        branch: "main",
        parent: None,
        summary: "Upload",
        description: "",
        ops,
    }
}

fn update(name: &str, old: Option<Oid>, new: Option<Oid>) -> RefUpdate {
    RefUpdate {
        name: name.to_owned(),
        old,
        new,
    }
}

/// A commit on top of `main` adding `secret.txt`, committed in its own pack
/// as `refs/heads/gone` and then deleted, as a force-push away leaves it:
/// in a committed pack, reachable from no ref.
struct Gone {
    commit: Oid,
    tree: Oid,
    blob: Oid,
}

async fn unreachable(world: &World, repo: RepoId, src: &Path) -> Gone {
    let blob = blob(src, SECRET);
    let main = rev(src, MAIN);
    let commit = commit_tree(src, MAIN, &[main], &[("secret.txt", blob)]);
    let tree = rev(src, &format!("{commit}^{{tree}}"));
    let out = world.dir.path().join(format!("gone-{commit}"));
    let pack = pack_objects(src, &format!("{commit}\n^{main}"), &out);
    world
        .commit(
            repo,
            &pack,
            1_761_400_100,
            &[set("refs/heads/gone", None, commit)],
        )
        .await;
    let generation = ok(world.index.snapshot(repo).await).full_repack_gen;
    ok(world
        .index
        .commit(CommitTx {
            repo,
            updates: &[update("refs/heads/gone", Some(commit), None)],
            pack: None,
            full_repack_gen: generation,
            actor: "tester",
            request: "force-push-away",
        })
        .await);
    Gone { commit, tree, blob }
}

async fn ref_log(world: &World, repo: RepoId, since: u64) -> Vec<RefLogRow> {
    ok(world.index.ref_log_since(repo, since).await)
}

async fn last_seq(world: &World, repo: RepoId) -> u64 {
    ref_log(world, repo, 0).await.last().map_or(0, |r| r.seq)
}

async fn pack_rows(world: &World, repo: RepoId) -> Vec<(String, String)> {
    let pool = world.sql().await;
    let rows: Vec<(String, String)> = ok(sqlx::query_as(
        "SELECT name, state FROM packs WHERE repo = ? ORDER BY created_at, name",
    )
    .bind(&repo.0[..])
    .fetch_all(&pool)
    .await);
    rows
}

/// `git {args}` in a fresh snapshot of `repo` on `node`; stdout.
async fn in_snapshot(node: &Engine, repo: RepoId, args: &[&str]) -> String {
    let snap = ok(node.snapshot(repo).await);
    let out = ok(git_command(node.git(), &snap, None)
        .args(args)
        .output()
        .await);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A full repack of `revs` (from the fixture `src`, which must hold every
/// object the index's refs reach) published through
/// `GitStore::publish_repack`, which bumps the full-repack generation. Task
/// 9's repack is built concurrently, so the test drives the index directly;
/// GC of the superseded inputs is simulated by deleting their files from
/// the bucket.
async fn full_repack(world: &World, repo: RepoId, src: &Path, revs: &str) {
    let snap = ok(world.index.snapshot(repo).await);
    let out = world
        .dir
        .path()
        .join(format!("full-{repo}-{}", snap.full_repack_gen));
    ok(std::fs::create_dir_all(&out));
    let sum = git_with_input(
        src,
        &[
            "pack-objects",
            "--revs",
            "-q",
            &out.join("pack").to_string_lossy(),
        ],
        &format!("{revs}\n"),
    );
    let pack = PackFiles {
        name: format!("pack-{}", sum.trim()),
        dir: out,
    };
    let row = world.pending(repo, &pack, 1_761_500_000).await;
    let lease = ok(world
        .index
        .acquire_lease(repo, "repacker", Duration::from_mins(1))
        .await)
    .unwrap_or_else(|| panic!("the repack lease is free"));
    let high_water = last_seq(world, repo).await;
    let inputs: Vec<_> = snap.packs.iter().map(|p| p.id).collect();
    ok(world
        .index
        .publish_repack(RepackPublish {
            repo,
            holder: "repacker",
            epoch: lease.epoch,
            outputs: &[row.id],
            inputs: &inputs,
            ref_log_high_water: Some(high_water),
        })
        .await);
    let after = ok(world.index.snapshot(repo).await).full_repack_gen;
    assert_eq!(after, snap.full_repack_gen + 1);
    let hex = ok(RepoIdHex::parse(&repo.to_string()));
    // An input holding exactly the output's objects has the output's name,
    // and so its files.
    for input in snap.packs.iter().filter(|p| p.name != pack.name) {
        for ext in [PackExt::Pack, PackExt::Idx, PackExt::Rev] {
            let key = Key::Pack {
                repo: hex,
                name: ok(PackName::parse(&input.name)),
                ext,
            };
            remove_from_bucket(&world.dir.path().join("bucket"), &key);
        }
    }
}

/// Deletes the file holding `key` from a local bucket, as GC would.
fn remove_from_bucket(root: &Path, key: &Key) {
    let path = root.join(key.to_string());
    if path.exists() {
        ok(std::fs::remove_file(&path));
    }
}

/// The object ids a committed pack's `.idx` in the bucket lists.
async fn pack_objects_in_bucket(world: &World, repo: RepoId, name: &str) -> BTreeSet<Oid> {
    let key = Key::Pack {
        repo: ok(RepoIdHex::parse(&repo.to_string())),
        name: ok(PackName::parse(name)),
        ext: PackExt::Idx,
    };
    let idx = ok(world.bucket.get(&key).await).unwrap_or_else(|| panic!("{name}.idx"));
    let dir = ok(tempfile::TempDir::new());
    let listing = {
        use std::io::Write as _;
        let mut child = ok(std::process::Command::new("git")
            .current_dir(dir.path())
            .arg("show-index")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn());
        if let Some(mut stdin) = child.stdin.take() {
            ok(stdin.write_all(&idx));
        }
        String::from_utf8_lossy(&ok(child.wait_with_output()).stdout).into_owned()
    };
    listing
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .map(|o| ok(o.parse()))
        .collect()
}

fn is_revision_not_found(e: &GitError) -> bool {
    matches!(e, GitError::RevisionNotFound { .. })
}

/// `pointer_canonical_form` (ADR 0005 item 9): openxet writes exactly the
/// three keys, each line `\n`-terminated; it reads pointers with unknown
/// keys but never writes them, and refuses anything else as a pointer.
#[tokio::test(flavor = "multi_thread")]
async fn pointer_canonical_form() {
    let sha = [0xab; 32];
    let hex = "ab".repeat(32);
    let canonical =
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{hex}\nsize 12345\n");
    assert_eq!(canonical_pointer(&sha, 12345), canonical.as_bytes());
    let pointer = Pointer {
        sha256: sha,
        size: 12345,
    };
    assert_eq!(parse_pointer(canonical.as_bytes()), Some(pointer));
    assert_eq!(pointer.sha256_hex(), hex);
    let extended = format!(
        "version https://git-lfs.github.com/spec/v1\next-0-foo sha256:{}\noid sha256:{hex}\nsize 12345\n",
        "cd".repeat(32)
    );
    assert_eq!(parse_pointer(extended.as_bytes()), Some(pointer));

    let v = "version https://git-lfs.github.com/spec/v1";
    let bad = [
        String::new(),
        format!("oid sha256:{hex}\nsize 1\n"),
        format!("version https://example.com/spec/v9\noid sha256:{hex}\nsize 1\n"),
        format!("{v}\noid sha256:{}\nsize 1\n", hex.to_uppercase()),
        format!("{v}\noid sha256:{}\nsize 1\n", &hex[..62]),
        format!("{v}\noid sha1:{}\nsize 1\n", &hex[..40]),
        format!("{v}\noid sha256:{hex}\nsize -1\n"),
        format!("{v}\noid sha256:{hex}\nsize 1x\n"),
        format!("{v}\noid sha256:{hex}\n"),
        format!("{v}\nsize 1\n"),
        format!("{v}\noid sha256:{hex}\noid sha256:{hex}\nsize 1\n"),
        format!("{v}\r\noid sha256:{hex}\r\nsize 1\r\n"),
        format!("{v}\nBad-Key x\noid sha256:{hex}\nsize 1\n"),
        format!("{v}\noid sha256:{hex}\nsize 1\nx {}\n", "y".repeat(1024)),
    ];
    for text in &bad {
        assert_eq!(parse_pointer(text.as_bytes()), None, "{text:?}");
    }

    // A Hub commit writes the canonical form; a stored pointer with an
    // unknown key reads as a pointer and stays byte for byte as it was.
    let world = World::new().await;
    let (repo, _fx) = seeded(&world, 5).await;
    let node = node(&world, "node").await;
    let commit = ok(node
        .hub_commit(
            repo,
            &principal("alice"),
            upload(vec![
                Op::LfsFile {
                    path: "weights/model.safetensors".to_owned(),
                    sha256: sha,
                    size: 12345,
                },
                file("ext.bin", extended.as_bytes()),
            ]),
        )
        .await);
    let snap = ok(node.snapshot(repo).await);
    let Blob::Lfs { oid, pointer: read } = ok(node
        .read_blob(&snap, commit, "weights/model.safetensors")
        .await)
    else {
        panic!("an LFS pointer reads as one");
    };
    assert_eq!(read, pointer);
    let stored = in_snapshot(&node, repo, &["cat-file", "blob", &oid.to_string()]).await;
    assert_eq!(stored, canonical);
    let Blob::Lfs { oid, pointer: read } = ok(node.read_blob(&snap, commit, "ext.bin").await)
    else {
        panic!("a pointer with an unknown key reads as one");
    };
    assert_eq!(read, pointer);
    let stored = in_snapshot(&node, repo, &["cat-file", "blob", &oid.to_string()]).await;
    assert_eq!(stored, extended);
    let root = ok(node.read_tree(&snap, commit, "", false).await);
    let ext = root
        .iter()
        .find(|e| e.path == "ext.bin")
        .unwrap_or_else(|| panic!("ext.bin is listed: {root:?}"));
    assert!(
        matches!(ext.kind, EntryKind::File { lfs: Some(p), size, .. } if p == pointer && size == extended.len() as u64),
        "{ext:?}"
    );
}

/// `hub_commit_builds_expected_tree` (ruling M2-R7): files, LFS pointers,
/// deletes, nested folders and a folder delete that removes every entry
/// under it; the author is the subject (or the `email` claim), the
/// committer openxet, the message the summary, a blank line and the
/// description; a fresh snapshot passes `git fsck --strict`.
#[tokio::test(flavor = "multi_thread")]
async fn hub_commit_builds_expected_tree() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let src = fixture_repo(&fx);
    let node = node(&world, "node").await;
    let tip = rev(&src, MAIN);
    let before: BTreeSet<String> = git(&src, &["ls-tree", "-r", "--name-only", MAIN])
        .lines()
        .map(str::to_owned)
        .collect();

    let commit = ok(node
        .hub_commit(
            repo,
            &principal("alice"),
            HubCommit {
                branch: "main",
                parent: Some(tip),
                summary: "Add files",
                description: "Many\nlines",
                ops: vec![
                    file("README.md", b"# hello\n"),
                    file("deep/a/b/c.txt", b"nested\n"),
                    Op::LfsFile {
                        path: "weights/model.safetensors".to_owned(),
                        sha256: [7; 32],
                        size: 1 << 30,
                    },
                    Op::DeleteFile("src/dir0/file0.txt".to_owned()),
                    Op::DeleteFolder("src/dir1".to_owned()),
                    file("src/dir2/file2.txt", b"rewritten\n"),
                ],
            },
        )
        .await);
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(commit));

    let snap = ok(node.snapshot(repo).await);
    let entries = ok(node.read_tree(&snap, commit, "", true).await);
    let files: BTreeSet<String> = entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::File { .. }))
        .map(|e| e.path.clone())
        .collect();
    let mut expected: BTreeSet<String> = before
        .iter()
        .filter(|p| *p != "src/dir0/file0.txt" && !p.starts_with("src/dir1/"))
        .cloned()
        .collect();
    expected
        .extend(["README.md", "deep/a/b/c.txt", "weights/model.safetensors"].map(str::to_owned));
    assert_eq!(files, expected);
    let dirs: BTreeSet<&str> = entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Dir { .. }))
        .map(|e| e.path.as_str())
        .collect();
    assert!(dirs.is_superset(&BTreeSet::from([
        "deep", "deep/a", "deep/a/b", "src", "weights"
    ])));
    assert!(!dirs.contains("src/dir1"), "a deleted folder is gone");
    assert!(
        entries.windows(2).all(|w| w[0].path < w[1].path),
        "entries are ordered by path"
    );
    let Blob::Inline { bytes, .. } = ok(node.read_blob(&snap, commit, "src/dir2/file2.txt").await)
    else {
        panic!("a small file reads inline");
    };
    assert_eq!(&bytes[..], b"rewritten\n");

    let raw = in_snapshot(&node, repo, &["cat-file", "commit", &commit.to_string()]).await;
    let (headers, message) = raw.split_once("\n\n").unwrap_or_default();
    assert_eq!(message, "Add files\n\nMany\nlines\n");
    let headers: Vec<&str> = headers.lines().collect();
    assert_eq!(headers[1], format!("parent {tip}"));
    assert!(
        headers[2].starts_with("author alice <alice@users.noreply.openxet.com> ")
            && headers[2].ends_with(" +0000"),
        "{}",
        headers[2]
    );
    assert!(headers[3].starts_with("committer openxet <noreply@openxet.com> "));
    fsck(&node, repo).await;
    fsck(&*self::node(&world, "fresh").await, repo).await;

    let mut bob = principal("bob");
    bob.email = Some("bob@example.org".to_owned());
    let second = ok(node
        .hub_commit(
            repo,
            &bob,
            HubCommit {
                branch: "refs/heads/main",
                parent: None,
                summary: "Just a summary",
                description: "",
                ops: vec![file("bob.txt", b"bob\n")],
            },
        )
        .await);
    let raw = in_snapshot(&node, repo, &["cat-file", "commit", &second.to_string()]).await;
    assert!(raw.contains("\nauthor bob <bob@example.org> "), "{raw}");
    assert!(raw.ends_with("\n\nJust a summary\n"), "{raw}");
}

/// What a refused request's error must be.
type Expected = fn(&GitError) -> bool;

/// Hub commits that can't be applied (a missing file or folder to delete,
/// a file in the way of a path or a folder in the way of a file, a path
/// git refuses, a missing or malformed branch, a stale parent) are refused
/// and change nothing.
#[tokio::test(flavor = "multi_thread")]
async fn hub_commit_refusals_change_nothing() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 5).await;
    let src = fixture_repo(&fx);
    let node = node(&world, "node").await;
    let first = rev(&src, MAIN);
    let tip = ok(node
        .hub_commit(
            repo,
            &principal("alice"),
            upload(vec![file("README.md", b"r\n")]),
        )
        .await);

    let seq = last_seq(&world, repo).await;
    let refused: Vec<(HubCommit<'static>, Expected)> = vec![
        (upload(vec![Op::DeleteFile("absent.txt".to_owned())]), |e| {
            matches!(e, GitError::EntryNotFound { .. })
        }),
        (
            upload(vec![Op::DeleteFolder("README.md".to_owned())]),
            |e| matches!(e, GitError::EntryNotFound { .. }),
        ),
        (upload(vec![Op::DeleteFile("src".to_owned())]), |e| {
            matches!(e, GitError::EntryNotFound { .. })
        }),
        (upload(vec![file("README.md/x", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (upload(vec![file("src", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (upload(vec![file("../x", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (upload(vec![file("a//b", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (upload(vec![file("/a", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (upload(vec![file(".git/config", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (upload(vec![file("sub/.GIT", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (upload(vec![file("", b"x")]), |e| {
            matches!(e, GitError::InvalidHubWrite(_))
        }),
        (
            HubCommit {
                branch: "nope",
                ..upload(vec![file("x", b"x")])
            },
            is_revision_not_found,
        ),
        (
            HubCommit {
                branch: "bad..name",
                ..upload(vec![file("x", b"x")])
            },
            |e| matches!(e, GitError::InvalidHubWrite(_)),
        ),
        (
            HubCommit {
                parent: Some(first),
                ..upload(vec![file("x", b"x")])
            },
            |e| matches!(e, GitError::ParentMismatch { .. }),
        ),
    ];
    for (req, expected) in refused {
        let ops = format!("{:?}", req.ops);
        let e = node
            .hub_commit(repo, &principal("alice"), req)
            .await
            .err()
            .unwrap_or_else(|| panic!("{ops} was accepted"));
        assert!(expected(&e), "{ops}: {e}");
    }
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(tip));
    assert_eq!(ref_log(&world, repo, seq).await, Vec::new());
}

/// `hub_read_by_sha_obeys_r22` (design §5.9 req 8, ADR 0004 item 6): reads
/// at a commit id succeed for a reachable commit that is no tip, and answer
/// "revision not found" alike for a force-pushed-away commit, a tree or
/// blob id, an absent object and a commit beyond the walk budget. Ref
/// names resolve, tags peeled.
#[tokio::test(flavor = "multi_thread")]
async fn hub_read_by_sha_obeys_r22() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 30).await;
    let src = fixture_repo(&fx);
    let gone = unreachable(&world, repo, &src).await;
    let node = node(&world, "node").await;
    let snap = ok(node.snapshot(repo).await);

    let deep = rev(&src, "main~5");
    let resolved = ok(node
        .resolve_revision(&snap, Revision::Oid(deep), READER)
        .await);
    assert_eq!(resolved.commit, deep);
    assert_eq!(resolved.tree, rev(&src, "main~5^{tree}"));
    assert!(!ok(node.read_tree(&snap, deep, "src", false).await).is_empty());
    let Blob::Inline { bytes, .. } = ok(node.read_blob(&snap, deep, "src/dir0/file0.txt").await)
    else {
        panic!("a fixture file reads inline");
    };
    assert_eq!(
        String::from_utf8_lossy(&bytes),
        git(&src, &["show", "main~5:src/dir0/file0.txt"])
    );
    let log = ok(node.log(&snap, deep, None, 3, READER).await);
    assert_eq!(
        log.iter().map(|c| c.oid).collect::<Vec<_>>(),
        vec![deep, rev(&src, "main~6"), rev(&src, "main~7")]
    );
    let tip = rev(&src, MAIN);
    let tag = rev(&src, "refs/tags/v1.0");
    for revision in [
        Revision::Oid(tag),
        Revision::Oid(tip),
        Revision::Ref("main"),
        Revision::Ref("refs/heads/main"),
        Revision::Ref("v1.0"),
        Revision::Ref("refs/tags/v1.0"),
        Revision::Ref("HEAD"),
    ] {
        let label = format!("{revision:?}");
        let r = ok(node.resolve_revision(&snap, revision, READER).await);
        assert_eq!(r.commit, tip, "{label}");
    }

    let absent = Oid([0x42; 20]);
    let main_tree = rev(&src, "main^{tree}");
    let main_blob = rev(&src, "main:src/dir0/file0.txt");
    let text_of = |e: &GitError, oid: Oid| e.to_string().replace(&oid.to_string(), "<rev>");
    let absent_text = {
        let e = node
            .resolve_revision(&snap, Revision::Oid(absent), READER)
            .await
            .err()
            .unwrap_or_else(|| panic!("an absent object resolved"));
        assert!(is_revision_not_found(&e), "{e}");
        text_of(&e, absent)
    };
    for oid in [gone.commit, gone.tree, gone.blob, main_tree, main_blob] {
        let e = node
            .resolve_revision(&snap, Revision::Oid(oid), READER)
            .await
            .err()
            .unwrap_or_else(|| panic!("{oid} resolved"));
        assert!(is_revision_not_found(&e), "{oid}: {e}");
        assert_eq!(
            text_of(&e, oid),
            absent_text,
            "indistinguishable from absent"
        );
    }
    for name in ["nope", "refs/heads/gone", "refs/heads/main/x"] {
        let e = node
            .resolve_revision(&snap, Revision::Ref(name), READER)
            .await
            .err()
            .unwrap_or_else(|| panic!("{name} resolved"));
        assert!(is_revision_not_found(&e), "{name}: {e}");
    }

    let mut tight = config(&world, "tight");
    tight.reachability = Budget {
        max_objects: 3,
        ..Budget::REACHABILITY
    };
    let tight = engine(&world, tight).await;
    let snap = ok(tight.snapshot(repo).await);
    ok(tight
        .resolve_revision(&snap, Revision::Oid(rev(&src, "main~1")), READER)
        .await);
    let e = tight
        .resolve_revision(&snap, Revision::Oid(rev(&src, "main~10")), READER)
        .await
        .err()
        .unwrap_or_else(|| panic!("a commit beyond the budget resolved"));
    assert!(is_revision_not_found(&e), "{e}");
}

/// The log walks within `git.reachability`'s budget, skipped commits
/// included; a read by id takes the reader's per-principal R22 slot, a ref
/// read doesn't, and a log page after a cursor takes the slots too.
#[tokio::test(flavor = "multi_thread")]
async fn hub_log_budget_and_reader_slots() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 30).await;
    let src = fixture_repo(&fx);
    let deep = rev(&src, "main~5");
    let mut tight = config(&world, "tight");
    tight.reachability = Budget {
        max_objects: 3,
        ..Budget::REACHABILITY
    };
    let tight = engine(&world, tight).await;
    let snap = ok(tight.snapshot(repo).await);
    let tip = rev(&src, MAIN);
    let page = ok(tight.log(&snap, tip, None, 10, READER).await);
    assert_eq!(page.len(), 3, "a page ends where the budget does");
    let beyond = ok(tight
        .log(&snap, tip, Some(rev(&src, "main~5")), 10, READER)
        .await);
    assert_eq!(beyond, Vec::new(), "a cursor beyond the budget");

    // A read by id takes the reader's per-principal slot; a ref doesn't.
    let mut busy = config(&world, "busy");
    busy.reachability = Budget {
        max_concurrent_per_principal: Some(0),
        ..Budget::REACHABILITY
    };
    let busy = engine(&world, busy).await;
    let snap = ok(busy.snapshot(repo).await);
    let e = busy
        .resolve_revision(&snap, Revision::Oid(deep), READER)
        .await
        .err();
    assert!(matches!(e, Some(GitError::Busy)), "{e:?}");
    ok(busy
        .resolve_revision(&snap, Revision::Ref("main"), READER)
        .await);
    let e = busy.log(&snap, tip, Some(deep), 1, READER).await.err();
    assert!(matches!(e, Some(GitError::Busy)), "{e:?}");
}

/// Tree listings, blobs, the log with its cursor, and the last commit that
/// changed each path (for `expand=true`).
#[tokio::test(flavor = "multi_thread")]
#[expect(
    clippy::too_many_lines,
    reason = "one history read every way: tree, blob, log and last commits"
)]
async fn hub_reads_tree_blob_log_and_last_commit() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let src = fixture_repo(&fx);
    let node = node(&world, "node").await;
    let seed = rev(&src, MAIN);
    let who = principal("erin");
    let c1 = ok(node
        .hub_commit(
            repo,
            &who,
            HubCommit {
                summary: "Add docs",
                description: "Two files.",
                ..upload(vec![file("docs/a.txt", b"a1"), file("docs/b.txt", b"b1")])
            },
        )
        .await);
    let c2 = ok(node
        .hub_commit(repo, &who, upload(vec![file("docs/a.txt", b"a2")]))
        .await);
    let pointer = Pointer {
        sha256: [9; 32],
        size: 77,
    };
    let c3 = ok(node
        .hub_commit(
            repo,
            &who,
            upload(vec![Op::LfsFile {
                path: "w.bin".to_owned(),
                sha256: pointer.sha256,
                size: pointer.size,
            }]),
        )
        .await);

    let snap = ok(node.snapshot(repo).await);
    assert_eq!(
        ok(node
            .resolve_revision(&snap, Revision::Ref("main"), READER)
            .await)
        .commit,
        c3
    );
    let docs = ok(node.read_tree(&snap, c3, "docs", false).await);
    let listed: Vec<(&str, u64)> = docs
        .iter()
        .map(|e| match &e.kind {
            EntryKind::File {
                size, lfs: None, ..
            } => (e.path.as_str(), *size),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(listed, vec![("docs/a.txt", 2), ("docs/b.txt", 2)]);
    assert_eq!(ok(node.read_tree(&snap, c3, "/docs/", false).await), docs);
    let root = ok(node.read_tree(&snap, c3, "", false).await);
    assert!(root.iter().any(|e| e.path == "w.bin"
        && matches!(e.kind, EntryKind::File { lfs: Some(p), .. } if p == pointer)));
    assert!(root.iter().all(|e| !e.path.contains('/')), "not recursive");
    for path in ["docs/a.txt", "nope", "../src", "docs/../src", "./docs"] {
        let e = node.read_tree(&snap, c3, path, false).await.err();
        assert!(
            matches!(e, Some(GitError::EntryNotFound { .. })),
            "{path}: {e:?}"
        );
    }
    // The size, from the object header, without inflating the blob.
    assert_eq!(ok(node.blob_size(&snap, c3, "docs/a.txt").await), 2);
    for path in ["docs", "", "nope.txt", "docs/a.txt/x"] {
        let e = node.read_blob(&snap, c3, path).await.err();
        assert!(
            matches!(e, Some(GitError::EntryNotFound { .. })),
            "{path}: {e:?}"
        );
        let e = node.blob_size(&snap, c3, path).await.err();
        assert!(
            matches!(e, Some(GitError::EntryNotFound { .. })),
            "{path}: {e:?}"
        );
    }

    let page = ok(node.log(&snap, c3, None, 2, READER).await);
    assert_eq!(page.iter().map(|c| c.oid).collect::<Vec<_>>(), vec![c3, c2]);
    let next = ok(node.log(&snap, c3, Some(c2), 2, READER).await);
    assert_eq!(
        next.iter().map(|c| c.oid).collect::<Vec<_>>(),
        vec![c1, seed]
    );
    let first = &next[0];
    assert_eq!(first.title(), "Add docs");
    assert_eq!(first.body(), "Two files.");
    assert_eq!(first.parents, vec![seed]);
    assert_eq!(
        (first.author.name.as_str(), first.author.email.as_str()),
        ("erin", "erin@users.noreply.openxet.com")
    );
    assert_eq!(first.committer.name, "openxet");
    assert!(first.committer.time > 1_761_000_000);

    let file0 = ok(git(
        &src,
        &["log", "-1", "--format=%H", MAIN, "--", "src/dir0/file0.txt"],
    )
    .trim()
    .parse::<Oid>());
    let last = ok(node
        .last_commit_for_paths(
            &snap,
            c3,
            &[
                "docs/a.txt",
                "docs/b.txt",
                "docs",
                "w.bin",
                "src/dir0/file0.txt",
                "missing",
            ],
        )
        .await);
    let oids: Vec<Option<Oid>> = last.iter().map(|c| c.as_ref().map(|c| c.oid)).collect();
    assert_eq!(
        oids,
        vec![Some(c2), Some(c1), Some(c2), Some(c3), Some(file0), None]
    );
}

/// `hub_commit_on_snapshot` (ADR 0004 item 6): with a superseded pack still
/// in the node cache that holds an object no committed pack has, a Hub
/// commit can neither read nor reference it; committing the same bytes
/// sends them. A Hub commit whose objects aren't connected in the snapshot
/// fails before the commit transaction, and nothing it wrote is visible.
#[tokio::test(flavor = "multi_thread")]
async fn hub_commit_on_snapshot() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let src = fixture_repo(&fx);
    let gone = unreachable(&world, repo, &src).await;
    let node = node(&world, "node").await;
    // Hydrates every committed pack, the one holding `gone` included.
    drop(ok(node.snapshot(repo).await));
    full_repack(&world, repo, &src, "refs/heads/main\nrefs/tags/v1.0").await;

    let snap = ok(node.snapshot(repo).await);
    let e = node
        .resolve_revision(&snap, Revision::Oid(gone.commit), READER)
        .await
        .err();
    assert!(
        matches!(e, Some(GitError::RevisionNotFound { .. })),
        "{e:?}"
    );
    let visible = ok(git_command(node.git(), &snap, None)
        .args(["cat-file", "-e", &gone.blob.to_string()])
        .output()
        .await);
    assert!(
        !visible.status.success(),
        "the superseded pack isn't linked"
    );
    drop(snap);
    let e = node
        .hub_commit(
            repo,
            &principal("alice"),
            HubCommit {
                parent: Some(gone.commit),
                ..upload(vec![file("x.txt", b"x")])
            },
        )
        .await
        .err();
    assert!(matches!(e, Some(GitError::ParentMismatch { .. })), "{e:?}");

    let commit = ok(node
        .hub_commit(
            repo,
            &principal("alice"),
            upload(vec![file("secret.txt", SECRET)]),
        )
        .await);
    let rows = pack_rows(&world, repo).await;
    let (newest, state) = rows.last().cloned().unwrap_or_default();
    assert_eq!(state, "committed");
    assert!(
        pack_objects_in_bucket(&world, repo, &newest)
            .await
            .contains(&gone.blob),
        "the commit sent the blob itself"
    );
    fsck(&*self::node(&world, "fresh").await, repo).await;

    let seq = last_seq(&world, repo).await;
    let rows = pack_rows(&world, repo).await;
    let e = node
        .hub_commit(
            repo,
            &principal(FAULT_OMIT_BLOBS_ACTOR),
            upload(vec![file("never-sent.txt", b"not in the pack\n")]),
        )
        .await;
    assert!(e.is_err(), "{e:?}");
    assert_eq!(
        pack_rows(&world, repo).await,
        rows,
        "no pack row was written"
    );
    assert_eq!(ref_log(&world, repo, seq).await, Vec::new());
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(commit));
}

/// A Hub commit whose pack row the pack sweep refuses, because it holds
/// the deletion claim of a pack of the same name, runs again a bounded
/// number of times and then answers retryable `Contended` with nothing
/// written; once the claim is released the same commit lands.
#[tokio::test(flavor = "multi_thread")]
async fn hub_commit_retries_while_its_pack_is_being_deleted() {
    let world = World::new().await;
    let (repo, _fx) = seeded(&world, 3).await;
    let node = node(&world, "node").await;
    let who = principal("dora");
    let before = db_ref(&world, repo, MAIN).await;
    let rows = pack_rows(&world, repo).await;

    let mut hold = node.hold_next_hub_write();
    let task = {
        let (node, who) = (node.clone(), who.clone());
        tokio::spawn(async move {
            node.hub_commit(repo, &who, upload(vec![file("d.txt", b"d\n")]))
                .await
        })
    };
    assert!(hold.reached().await);
    // The held attempt's pack, in its snapshot's quarantine.
    let name = quarantined_pack(&world.dir.path().join("node"))
        .unwrap_or_else(|| panic!("the held write has a quarantined pack"));
    assert!(ok(world.index.claim_pack_deletion(repo, &name).await));
    hold.release();
    let e = ok(task.await).err();
    assert!(matches!(e, Some(GitError::Contended { .. })), "{e:?}");
    assert_eq!(db_ref(&world, repo, MAIN).await, before);
    assert_eq!(pack_rows(&world, repo).await, rows, "no pack row written");

    ok(world.index.release_pack_deletion(repo, &name).await);
    let landed = ok(node
        .hub_commit(repo, &who, upload(vec![file("d.txt", b"d\n")]))
        .await);
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(landed));
}

/// The name of the first pack under `dir`'s `objects/quarantine/pack`
/// directories (a Hub write's, in its snapshot).
fn quarantined_pack(dir: &Path) -> Option<String> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.ends_with("objects/quarantine/pack") {
            for file in std::fs::read_dir(&path).ok()?.flatten() {
                let name = file.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name.strip_suffix(".pack") {
                    return Some(stem.to_owned());
                }
            }
        } else if path.is_dir()
            && let Some(found) = quarantined_pack(&path)
        {
            return Some(found);
        }
    }
    None
}

/// A Hub commit waits for the commit to its branch already running on its
/// node at most `hub.commit_wait`, then is refused as retryable `Busy`;
/// another branch doesn't wait.
#[tokio::test(flavor = "multi_thread")]
async fn hub_commit_wait_is_bounded() {
    let world = World::new().await;
    let (repo, _fx) = seeded(&world, 3).await;
    let mut short = config(&world, "short");
    short.hub_commit_wait = Duration::from_millis(200);
    let node = engine(&world, short).await;

    let mut hold = node.hold_next_hub_write();
    let first = {
        let node = node.clone();
        tokio::spawn(async move {
            node.hub_commit(repo, &principal("ann"), upload(vec![file("a", b"a")]))
                .await
        })
    };
    assert!(hold.reached().await);
    let e = node
        .hub_commit(repo, &principal("ben"), upload(vec![file("b", b"b")]))
        .await
        .err();
    assert!(matches!(e, Some(GitError::Busy)), "{e:?}");
    let mut elsewhere = upload(vec![file("c", b"c")]);
    elsewhere.branch = "refs/heads/v";
    let e = node
        .hub_commit(repo, &principal("cal"), elsewhere)
        .await
        .err();
    assert!(
        matches!(e, Some(GitError::RevisionNotFound { .. })),
        "another branch takes its own turn: {e:?}"
    );
    hold.release();
    let landed = ok(ok(first.await));
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(landed));
}

/// A Hub commit to a repository with no refs is its root commit (how
/// Task 12 writes M2-R5's initial commit): no parent, the branch created,
/// readable and `fsck`-clean on a fresh node. A `parentCommit` on an empty
/// repository is a mismatch, and once the branch exists a missing branch
/// is not found.
#[tokio::test(flavor = "multi_thread")]
async fn hub_root_commit_on_empty_repository() {
    let world = World::new().await;
    let repo = world.repo("org", "empty").await;
    let node = node(&world, "node").await;
    let who = principal("rosa");

    let mut with_parent = upload(vec![file("README.md", b"# hi\n")]);
    with_parent.parent = Some(Oid([7; 20]));
    let e = node.hub_commit(repo, &who, with_parent).await.err();
    assert!(
        matches!(e, Some(GitError::ParentMismatch { actual: None, .. })),
        "{e:?}"
    );
    assert_eq!(db_ref(&world, repo, MAIN).await, None);

    let root = ok(node
        .hub_commit(repo, &who, upload(vec![file("README.md", b"# hi\n")]))
        .await);
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(root));
    let snap = ok(node.snapshot(repo).await);
    let log = ok(node.log(&snap, root, None, 10, READER).await);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].oid, root);
    assert_eq!(log[0].parents, Vec::new());
    let Blob::Inline { bytes, .. } = ok(node.read_blob(&snap, root, "README.md").await) else {
        panic!("a small file reads inline");
    };
    assert_eq!(&bytes[..], b"# hi\n");
    let fresh = self::node(&world, "fresh").await;
    fsck(&fresh, repo).await;

    let mut other = upload(vec![file("x", b"x")]);
    other.branch = "other";
    let e = node.hub_commit(repo, &who, other).await.err();
    assert!(
        matches!(e, Some(GitError::RevisionNotFound { .. })),
        "{e:?}"
    );
}

/// `concurrent_hub_commits_both_land` (Review Focus 2, ADR 0004 item 3):
/// eight Hub commits to `main` on one node all land, one after another; a
/// commit on a second node held after its snapshot loses the ref race when
/// released, retries from a fresh snapshot with a new pack row, and lands
/// on top of all eight.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_hub_commits_both_land() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let src = fixture_repo(&fx);
    let seed = rev(&src, MAIN);
    let a = node(&world, "a").await;
    let b = node(&world, "b").await;

    let mut hold = b.hold_next_hub_write();
    let bob = {
        let b = b.clone();
        tokio::spawn(async move {
            b.hub_commit(repo, &principal("bob"), upload(vec![file("b.txt", b"b\n")]))
                .await
        })
    };
    assert!(hold.reached().await, "the held commit took its snapshot");

    let tasks: Vec<_> = (0..8)
        .map(|i| {
            let a = a.clone();
            tokio::spawn(async move {
                let path = format!("f{i}.txt");
                a.hub_commit(
                    repo,
                    &principal(&format!("user{i}")),
                    upload(vec![file(&path, path.as_bytes())]),
                )
                .await
            })
        })
        .collect();
    let mut landed = Vec::new();
    for task in tasks {
        landed.push(ok(ok(task.await)));
    }
    hold.release();
    let last = ok(ok(bob.await));
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(last));

    let snap = ok(a.snapshot(repo).await);
    let log = ok(a.log(&snap, last, None, 10, READER).await);
    assert_eq!(log[9].oid, seed);
    for pair in log.windows(2) {
        assert_eq!(pair[0].parents, vec![pair[1].oid], "a linear history");
    }
    let mut ours: Vec<Oid> = log[1..9].iter().map(|c| c.oid).collect();
    ours.sort_unstable();
    landed.sort_unstable();
    assert_eq!(ours, landed, "every commit landed");
    let root: BTreeSet<String> = ok(a.read_tree(&snap, last, "", false).await)
        .into_iter()
        .map(|e| e.path)
        .collect();
    for i in 0..8 {
        assert!(
            root.contains(&format!("f{i}.txt")),
            "the retry saw f{i}.txt"
        );
    }
    assert!(root.contains("b.txt"));

    let rows = pack_rows(&world, repo).await;
    let states = |s: &str| rows.iter().filter(|(_, st)| st == s).count();
    assert_eq!(states("committed"), 10, "{rows:?}");
    assert_eq!(states("lost"), 1, "the held attempt's pack: {rows:?}");
    let names: BTreeSet<&String> = rows.iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), rows.len(), "no pack row reused");
    fsck(&a, repo).await;
}

/// `ref_log_in_cas_transaction`, the Hub half (design §5.9 req 8): Hub
/// commits and Hub ref creates, updates and deletes write one ref-log row
/// each in their commit transaction, with the writer as actor; annotated
/// tags are tag objects built in-process; a refused write logs nothing.
#[tokio::test(flavor = "multi_thread")]
async fn ref_log_in_cas_transaction() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let src = fixture_repo(&fx);
    let tip = rev(&src, MAIN);
    let node = node(&world, "node").await;
    let who = principal("carol");
    let seq = last_seq(&world, repo).await;

    let c = ok(node
        .hub_commit(repo, &who, upload(vec![file("x.txt", b"x\n")]))
        .await);
    ok(node
        .hub_ref_write(repo, &who, update("refs/heads/dev", None, Some(c)), None)
        .await);
    ok(node
        .hub_ref_write(
            repo,
            &who,
            update("refs/heads/dev", Some(c), Some(tip)),
            None,
        )
        .await);
    ok(node
        .hub_ref_write(
            repo,
            &who,
            update("refs/tags/v2", None, Some(c)),
            Some(TagInfo {
                message: "release 2".to_owned(),
                tagger: Ident::author(&who),
            }),
        )
        .await);
    ok(node
        .hub_ref_write(repo, &who, update("refs/heads/dev", Some(tip), None), None)
        .await);

    let log = ref_log(&world, repo, seq).await;
    let tag = db_ref(&world, repo, "refs/tags/v2")
        .await
        .unwrap_or_else(|| panic!("the tag exists"));
    let got: Vec<(&str, Option<Oid>, Option<Oid>)> = log
        .iter()
        .map(|r| (r.name.as_str(), r.old, r.new))
        .collect();
    assert_eq!(
        got,
        vec![
            (MAIN, Some(tip), Some(c)),
            ("refs/heads/dev", None, Some(c)),
            ("refs/heads/dev", Some(c), Some(tip)),
            ("refs/tags/v2", None, Some(tag)),
            ("refs/heads/dev", Some(tip), None),
        ]
    );
    assert!(log.iter().all(|r| r.actor == "carol" && r.repo == repo));
    let requests: BTreeSet<&str> = log.iter().map(|r| r.request.as_str()).collect();
    assert_eq!(requests.len(), log.len(), "one request per write");

    assert_ne!(tag, c, "an annotated tag is its own object");
    let raw = in_snapshot(&node, repo, &["cat-file", "tag", &tag.to_string()]).await;
    assert!(
        raw.starts_with(&format!(
            "object {c}\ntype commit\ntag v2\ntagger carol <carol@users.noreply.openxet.com> "
        )),
        "{raw}"
    );
    assert!(raw.ends_with("\n\nrelease 2\n"), "{raw}");
    let snap = ok(node.snapshot(repo).await);
    assert_eq!(
        ok(node
            .resolve_revision(&snap, Revision::Ref("v2"), READER)
            .await)
        .commit,
        c
    );
    drop(snap);
    fsck(&node, repo).await;

    let seq = last_seq(&world, repo).await;
    let refused = [
        (update(MAIN, None, Some(tip)), None),
        (update("refs/heads/dev", Some(tip), None), None),
        (update(MAIN, Some(c), None), None),
        (update("refs/heads/x", None, Some(tag)), None),
        (update("refs/heads/a..b", None, Some(c)), None),
        (update("refs/heads/x", None, None), None),
        (
            update("refs/heads/y", None, Some(c)),
            Some(TagInfo {
                message: "on a branch".to_owned(),
                tagger: Ident::author(&who),
            }),
        ),
    ];
    for (u, tag) in refused {
        let label = format!("{u:?}");
        let e = node.hub_ref_write(repo, &who, u, tag).await;
        assert!(e.is_err(), "{label} was accepted");
    }
    assert_eq!(ref_log(&world, repo, seq).await, Vec::new());
}

/// `writer_cannot_make_unreachable_reachable`, the Hub routes (ADR 0004
/// item 3): an object in a committed pack that no ref reaches can't become
/// reachable by pointing a ref or a tag at it, naming it as a parent, or
/// naming it as a tree entry without sending it; the refusal reads like a
/// missing object, and the same commit succeeds when it sends the object.
#[tokio::test(flavor = "multi_thread")]
async fn writer_cannot_make_unreachable_reachable() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let src = fixture_repo(&fx);
    let tip = rev(&src, MAIN);
    let gone = unreachable(&world, repo, &src).await;
    let node = node(&world, "node").await;
    let who = principal("dave");
    let seq = last_seq(&world, repo).await;

    let absent = Oid([0x42; 20]);
    let tag = || {
        Some(TagInfo {
            message: "t".to_owned(),
            tagger: Ident::author(&who),
        })
    };
    let attempts = [
        (update("refs/heads/x", None, Some(gone.commit)), None),
        (update("refs/heads/x", None, Some(absent)), None),
        (update(MAIN, Some(tip), Some(gone.commit)), None),
        (update("refs/tags/t", None, Some(gone.commit)), None),
        (update("refs/tags/t", None, Some(gone.tree)), None),
        (update("refs/tags/t", None, Some(gone.blob)), None),
        (update("refs/tags/t", None, Some(gone.commit)), tag()),
    ];
    for (u, t) in attempts {
        let label = format!("{u:?}");
        let e = node
            .hub_ref_write(repo, &who, u, t)
            .await
            .err()
            .unwrap_or_else(|| panic!("{label} was accepted"));
        assert!(is_revision_not_found(&e), "{label}: {e}");
    }

    let e = node
        .hub_commit(
            repo,
            &who,
            HubCommit {
                parent: Some(gone.commit),
                ..upload(vec![file("x.txt", b"x")])
            },
        )
        .await
        .err();
    assert!(matches!(e, Some(GitError::ParentMismatch { .. })), "{e:?}");

    let e = node
        .hub_commit(
            repo,
            &principal(FAULT_OMIT_BLOBS_ACTOR),
            upload(vec![file("secret.txt", SECRET)]),
        )
        .await
        .err()
        .unwrap_or_else(|| panic!("a tree entry naming an unreachable blob was accepted"));
    assert!(matches!(e, GitError::MissingObjects), "{e}");
    assert!(e.to_string().starts_with(MISSING_OBJECTS), "{e}");
    assert_eq!(ref_log(&world, repo, seq).await, Vec::new());
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(tip));

    let commit = ok(node
        .hub_commit(repo, &who, upload(vec![file("secret.txt", SECRET)]))
        .await);
    assert_eq!(db_ref(&world, repo, MAIN).await, Some(commit));
    fsck(&node, repo).await;
}

/// `hub_ref_write_fenced` (design §5.9 req 8): a Hub ref create, update or
/// delete whose snapshot read an older full-repack generation fails on the
/// fence and retries from a fresh snapshot; a create or update to an object
/// no ref reaches is refused, a reachable commit that is no tip is
/// accepted, and a target in no committed pack is refused.
#[tokio::test(flavor = "multi_thread")]
async fn hub_ref_write_fenced() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let src = fixture_repo(&fx);
    let node = node(&world, "node").await;
    let who = principal("frank");
    let (deep, near) = (rev(&src, "main~3"), rev(&src, "main~1"));
    let revs = "refs/heads/main\nrefs/tags/v1.0";

    for (u, expected) in [
        (update("refs/heads/f", None, Some(deep)), Some(deep)),
        (update("refs/heads/f", Some(deep), Some(near)), Some(near)),
        (update("refs/heads/f", Some(near), None), None),
    ] {
        let generation = ok(world.index.snapshot(repo).await).full_repack_gen;
        let mut hold = node.hold_next_hub_write();
        let task = {
            let (node, who) = (node.clone(), who.clone());
            tokio::spawn(async move { node.hub_ref_write(repo, &who, u, None).await })
        };
        assert!(hold.reached().await);
        full_repack(&world, repo, &src, revs).await;
        hold.release();
        ok(ok(task.await));
        assert_eq!(db_ref(&world, repo, "refs/heads/f").await, expected);
        let rows = ref_log(&world, repo, 0).await;
        assert_eq!(rows.last().map(|r| r.new), Some(expected));
        assert_eq!(
            ok(world.index.snapshot(repo).await).full_repack_gen,
            generation + 1
        );
    }

    let gone = unreachable(&world, repo, &src).await;
    let e = node
        .hub_ref_write(
            repo,
            &who,
            update("refs/heads/g", None, Some(gone.commit)),
            None,
        )
        .await
        .err();
    assert!(
        matches!(e, Some(GitError::RevisionNotFound { .. })),
        "{e:?}"
    );

    // A commit only in a pending pack (another writer's, uncommitted).
    let blob = blob(&src, b"pending only\n");
    let pending = commit_tree(&src, MAIN, &[rev(&src, MAIN)], &[("p.txt", blob)]);
    let out = world.dir.path().join("pending");
    let pack = pack_objects(&src, &format!("{pending}\n^{MAIN}"), &out);
    world.pending(repo, &pack, 1_761_600_000).await;
    let e = node
        .hub_ref_write(
            repo,
            &who,
            update("refs/heads/p", None, Some(pending)),
            None,
        )
        .await
        .err();
    assert!(
        matches!(e, Some(GitError::RevisionNotFound { .. })),
        "{e:?}"
    );
    assert_eq!(db_ref(&world, repo, "refs/heads/g").await, None);
    assert_eq!(db_ref(&world, repo, "refs/heads/p").await, None);
}

/// `publish_then_cas_survives_faults`, the Hub-ref cases (ADR 0004, "Must
/// stay true"): a Hub `create_branch` and `create_tag` to an object id,
/// held between their snapshot (generation g) and their commit while a full
/// repack publishes g+1 and GC deletes the superseded inputs, fail on the
/// fence and retry from a fresh snapshot, and no ref points at a dropped
/// object. An object no ref has reached for longer than the grace period is
/// refused by the writer reachability rule before any transaction. The
/// test's object was made unreachable just before the write: it stands in
/// for one unreachable longer than the grace, since R22 refuses both alike
/// (reachability from the snapshot's refs has no grace).
#[tokio::test(flavor = "multi_thread")]
async fn publish_then_cas_survives_faults() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 20).await;
    let src = fixture_repo(&fx);
    let node = node(&world, "node").await;
    let who = principal("grace");
    let revs = "refs/heads/main\nrefs/tags/v1.0";

    let cases = [
        (
            update("refs/heads/b", None, Some(rev(&src, "main~4"))),
            None,
        ),
        (
            update("refs/tags/t", None, Some(rev(&src, "main~2"))),
            Some(TagInfo {
                message: "held".to_owned(),
                tagger: Ident::author(&who),
            }),
        ),
    ];
    for (u, tag) in cases {
        let name = u.name.clone();
        let target = u.new;
        // The node's cache holds the packs the repack supersedes.
        drop(ok(node.snapshot(repo).await));
        let mut hold = node.hold_next_hub_write();
        let task = {
            let (node, who) = (node.clone(), who.clone());
            tokio::spawn(async move { node.hub_ref_write(repo, &who, u, tag).await })
        };
        assert!(hold.reached().await);
        full_repack(&world, repo, &src, revs).await;
        hold.release();
        ok(ok(task.await));
        let now = db_ref(&world, repo, &name).await;
        assert!(now.is_some(), "{name} was created on the retry");
        let fresh = self::node(&world, &format!("fresh-{}", name.replace('/', "-"))).await;
        let peeled = in_snapshot(&fresh, repo, &["rev-parse", &format!("{name}^{{commit}}")]).await;
        assert_eq!(peeled.trim().parse::<Oid>().ok(), target);
        fsck(&fresh, repo).await;
        if name.starts_with("refs/tags/") {
            // The retry rebuilt the same tag object, so its pack has the
            // lost attempt's name: one row `lost`, one `committed`. Task 9's
            // sweep deletes a pack's files only when no `pending` or
            // `committed` row names them (`sweep_keeps_files_another_row_names`).
            let rows = pack_rows(&world, repo).await;
            let shared: Vec<&(String, String)> = rows
                .iter()
                .filter(|(n, _)| rows.iter().filter(|(m, _)| m == n).count() == 2)
                .collect();
            let mut states: Vec<&str> = shared.iter().map(|(_, s)| s.as_str()).collect();
            states.sort_unstable();
            assert_eq!(states, ["committed", "lost"], "{rows:?}");
        }
    }

    let gone = unreachable(&world, repo, &src).await;
    let seq = last_seq(&world, repo).await;
    let mut hold = node.hold_next_hub_write();
    let write = node.hub_ref_write(
        repo,
        &who,
        update("refs/heads/c", None, Some(gone.commit)),
        None,
    );
    let e = tokio::select! {
        result = write => result.err(),
        _ = hold.reached() => panic!("an unreachable target reached the commit transaction"),
    };
    assert!(
        matches!(e, Some(GitError::RevisionNotFound { .. })),
        "{e:?}"
    );
    assert_eq!(ref_log(&world, repo, seq).await, Vec::new());
    assert_eq!(db_ref(&world, repo, "refs/heads/c").await, None);
}
