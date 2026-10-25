//! `upload-pack` (ADR 0004 item 2): the filtered v2 advertisement and its
//! command allowlist, in-process `ls-refs` against git's own, v0/v1 wants
//! checked before git runs, and real `git clone` and `git fetch` over every
//! protocol version.
//!
//! The tests that must prove git never ran rerun one ignored test of this
//! binary in a child process whose `PATH` starts with a `git` that logs
//! each start (`common::fetch::run_child_with_git`).

mod common;

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;

use common::fetch::{
    client_ok, gits_under_test, is_child, not_our_ref, request, run_child_with_git, scenario,
    serve_http, upload, upload_pack_spawns, v0_fetch, v2_fetch,
};
use common::{World, git, ok, pack_objects, rev, set};
use openxet_git::{Engine, GitError, Pkt, Service, git_command, read_pkt};
use openxet_index::{IndexError, RepoId};
use tokio::io::AsyncWriteExt as _;

/// The data lines of a pkt-line message, without trailing newlines, up to
/// its flush.
fn lines(message: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Ok(Some((pkt, len))) = read_pkt(&message[pos..]) {
        pos += len;
        match pkt {
            Pkt::Data(d) => {
                let line = d.strip_suffix(b"\n").unwrap_or(d);
                out.push(String::from_utf8_lossy(line).into_owned());
            }
            Pkt::Flush => break,
            Pkt::Delim | Pkt::ResponseEnd => {}
        }
    }
    out
}

fn is_err(response: &[u8]) -> bool {
    response.get(4..8) == Some(b"ERR ")
}

#[test]
fn v2_command_allowlist() {
    for git in gits_under_test() {
        run_child_with_git("child_v2_command_allowlist", &git);
    }
}

#[tokio::test]
#[ignore = "run by v2_command_allowlist in a child process"]
async fn child_v2_command_allowlist() {
    if !is_child() {
        return;
    }
    let world = World::new().await;
    let engine = world.engine().await;
    let s = scenario(&world).await;

    let adv = ok(engine.advertise(s.repo, 2, Service::UploadPack).await);
    let lines = lines(&adv);
    let keys: BTreeSet<&str> = lines
        .iter()
        .map(|l| l.split_once('=').map_or(l.as_str(), |(k, _)| k))
        .collect();
    let allowed = BTreeSet::from([
        "version 2",
        "agent",
        "ls-refs",
        "fetch",
        "server-option",
        "object-format",
    ]);
    assert_eq!(keys, allowed, "advertisement: {lines:?}");
    let fetch = lines.iter().find(|l| l.starts_with("fetch")).cloned();
    assert!(
        fetch.is_some_and(|f| !f.split(['=', ' ']).any(|w| w == "filter")),
        "{lines:?}"
    );
    assert!(adv.ends_with(b"0000"));
    let unknown = RepoId::generate();
    assert!(matches!(
        engine.advertise(unknown, 2, Service::UploadPack).await,
        Err(GitError::Index(IndexError::UnknownRepo { repo })) if repo == unknown
    ));

    let tip = s.main.to_string();
    let refused: [Vec<u8>; 3] = [
        request(&[
            "command=object-info",
            "object-format=sha1",
            "0001",
            "size",
            &format!("oid {}", s.dangling),
            "0000",
        ]),
        request(&["command=bundle-uri", "object-format=sha1", "0001", "0000"]),
        request(&["command=nonsense", "0001", &format!("want {tip}"), "0000"]),
    ];
    for (req, name) in refused
        .into_iter()
        .zip(["object-info", "bundle-uri", "nonsense"])
    {
        let response = upload(&engine, s.repo, 2, req).await;
        let text = String::from_utf8_lossy(&response);
        assert!(is_err(&response), "{name}: {text}");
        assert!(
            text.contains(name) && text.contains("not supported"),
            "{text}"
        );
    }
    assert_eq!(upload_pack_spawns(), 0, "git ran for a refused command");

    let listed = upload(&engine, s.repo, 2, request(&["command=ls-refs", "0000"])).await;
    assert!(!is_err(&listed) && !listed.is_empty());
    assert_eq!(upload_pack_spawns(), 0, "ls-refs is answered in-process");

    let fetched = upload(&engine, s.repo, 2, v2_fetch(&[s.main])).await;
    assert!(
        fetched.windows(9).any(|w| w == b"packfile\n"),
        "{}",
        String::from_utf8_lossy(&fetched)
    );
    assert_eq!(upload_pack_spawns(), 1, "the spawn log counts a fetch");
}

#[test]
fn v0_wants_checked_too() {
    for git in gits_under_test() {
        run_child_with_git("child_v0_wants_checked_too", &git);
    }
}

#[tokio::test]
#[ignore = "run by v0_wants_checked_too in a child process"]
async fn child_v0_wants_checked_too() {
    if !is_child() {
        return;
    }
    let world = World::new().await;
    let engine = world.engine().await;
    let s = scenario(&world).await;
    for protocol in [0, 1] {
        for wants in [vec![s.dangling], vec![s.main, s.away]] {
            let refused = wants[wants.len() - 1];
            let response = upload(&engine, s.repo, protocol, v0_fetch(&wants)).await;
            assert_eq!(
                response,
                not_our_ref(refused),
                "v{protocol} {wants:?}: {}",
                String::from_utf8_lossy(&response)
            );
        }
        let malformed = request(&["want refs/heads/main", "0000", "done"]);
        assert!(is_err(&upload(&engine, s.repo, protocol, malformed).await));
    }
    assert_eq!(upload_pack_spawns(), 0, "git ran for a refused want");

    let wants = [s.main, s.at("main~12")];
    let response = upload(&engine, s.repo, 0, v0_fetch(&wants)).await;
    assert!(
        response.starts_with(b"0008NAK\n") && response.windows(4).any(|w| w == b"PACK"),
        "{}",
        String::from_utf8_lossy(&response)
    );
    assert_eq!(upload_pack_spawns(), 1);
}

/// git's own answer to `req` on a fresh snapshot of `repo`.
async fn git_answer(
    engine: &Engine,
    repo: RepoId,
    protocol: u8,
    args: &[&str],
    req: &[u8],
) -> Vec<u8> {
    let snapshot = ok(engine.snapshot(repo).await);
    let mut child = ok(git_command(engine.git(), &snapshot, Some(protocol))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn());
    if let Some(mut stdin) = child.stdin.take() {
        ok(stdin.write_all(req).await);
    }
    let out = ok(child.wait_with_output().await);
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[tokio::test]
async fn ls_refs_matches_git() {
    let world = World::new().await;
    let s = scenario(&world).await;
    let empty = world.repo("org", "empty").await;
    let engine = world.engine().await;
    let stateless = ["upload-pack", "--stateless-rpc", "."];

    let variants: &[&[&str]] = &[
        &[],
        &["peel"],
        &["symrefs"],
        &["peel", "symrefs"],
        &["peel", "symrefs", "unborn"],
        &["peel", "ref-prefix refs/tags/"],
        &["symrefs", "ref-prefix HEAD"],
        &["symrefs", "ref-prefix HEAD", "ref-prefix refs/heads/"],
        &["ref-prefix refs/heads/m", "ref-prefix refs/heads/"],
        &["ref-prefix refs/nothing/"],
        &["unborn"],
    ];
    for repo in [s.repo, empty] {
        for args in variants {
            let mut lines = vec![
                "command=ls-refs",
                "agent=git/2.39.5",
                "object-format=sha1",
                "0001",
            ];
            lines.extend_from_slice(args);
            lines.push("0000");
            let req = request(&lines);
            let ours = upload(&engine, repo, 2, req.clone()).await;
            let theirs = git_answer(&engine, repo, 2, &stateless, &req).await;
            assert_eq!(
                String::from_utf8_lossy(&ours),
                String::from_utf8_lossy(&theirs),
                "ls-refs {args:?} on {}",
                if repo == empty {
                    "the empty repo"
                } else {
                    "the scenario"
                }
            );
        }
    }

    // The same refs and peeled tags as git's v0 advertisement.
    let full = request(&["command=ls-refs", "0001", "peel", "symrefs", "0000"]);
    let mut listed = BTreeSet::new();
    for line in lines(&upload(&engine, s.repo, 2, full).await) {
        let mut words = line.split(' ');
        let (oid, name) = (
            words.next().unwrap_or_default(),
            words.next().unwrap_or_default(),
        );
        listed.insert(format!("{oid} {name}"));
        for word in words {
            if let Some(peeled) = word.strip_prefix("peeled:") {
                listed.insert(format!("{peeled} {name}^{{}}"));
            }
        }
    }
    let adv = ok(engine.advertise(s.repo, 0, Service::UploadPack).await);
    let advertised: BTreeSet<String> = lines(&adv)
        .iter()
        .map(|l| l.split('\0').next().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(listed, advertised);
    assert!(listed.contains(&format!("{} refs/tags/t15^{{}}", s.t15_target)));
    assert!(listed.contains(&format!("{} HEAD", s.main)));
}

/// Asserts `work`'s checkout holds exactly the files of `rev` in `src`,
/// byte for byte.
fn assert_checkout(src: &Path, rev: &str, work: &Path) {
    let files = git(src, &["ls-tree", "-r", "--name-only", rev]);
    let mut count = 0;
    for file in files.lines() {
        let want = git(src, &["show", &format!("{rev}:{file}")]);
        let got = ok(std::fs::read(work.join(file)));
        assert_eq!(got, want.as_bytes(), "{file} at {rev}");
        count += 1;
    }
    let tracked = client_ok(work, &["ls-files"]).lines().count();
    assert_eq!(tracked, count);
}

// Multi-threaded: the git client blocks this thread while the front
// serves it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_round_trip() {
    let world = World::new().await;
    let engine = Arc::new(world.engine().await);
    let fx = common::fixture("tf10k", 30);
    let src = common::fixture_repo(&fx);
    let packs = world.dir.path().join("packs");
    let base = pack_objects(&src, "main~10", &packs.join("base"));
    let ahead = pack_objects(&src, "main\nrefs/tags/v1.0\n^main~10", &packs.join("ahead"));
    let (old, tip, tag) = (rev(&src, "main~10"), rev(&src, "main"), rev(&src, "v1.0"));
    let work = ok(tempfile::TempDir::new());

    for protocol in [0u8, 1, 2] {
        let repo = world.repo("org", &format!("round-trip-v{protocol}")).await;
        world
            .commit(
                repo,
                &base,
                1_761_400_000,
                &[set("refs/heads/main", None, old)],
            )
            .await;
        let (url, seen) = serve_http(Arc::clone(&engine), repo).await;
        let version = format!("protocol.version={protocol}");
        let dest = format!("clone-v{protocol}");
        client_ok(work.path(), &["-c", &version, "clone", "-q", &url, &dest]);
        let clone = work.path().join(&dest);
        assert_eq!(
            client_ok(&clone, &["rev-parse", "HEAD"]).trim(),
            old.to_string()
        );
        assert_checkout(&src, "main~10", &clone);

        world
            .commit(
                repo,
                &ahead,
                1_761_400_001,
                &[
                    set("refs/heads/main", Some(old), tip),
                    set("refs/tags/v1.0", None, tag),
                ],
            )
            .await;
        client_ok(&clone, &["-c", &version, "fetch", "-q", "--tags", "origin"]);
        assert_eq!(
            client_ok(&clone, &["rev-parse", "origin/main", "refs/tags/v1.0"]),
            format!("{tip}\n{tag}\n")
        );
        client_ok(&clone, &["merge", "-q", "--ff-only", "origin/main"]);
        assert_checkout(&src, "main", &clone);
        client_ok(&clone, &["fsck", "--strict", "--no-progress"]);

        let seen = seen.lock().map(|s| s.protocols.clone()).unwrap_or_default();
        assert!(seen.len() >= 2, "v{protocol}: {seen:?}");
        assert!(seen.iter().all(|p| *p == protocol), "v{protocol}: {seen:?}");
    }
}
