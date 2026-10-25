//! The hardened `proc-receive` hook route (ADR 0004 item 7): a private
//! inherited channel with a single-use token, a hook that can't choose the
//! repository or principal, pack paths confined to the request's snapshot,
//! and policy re-checked by openxet whatever the hook relays.

mod common;
mod push_support;

use std::collections::BTreeMap;
use std::process::Stdio;

use openxet_git::HookServer;
use openxet_git::hook::{HOOK_TOKEN_ENV, HookContext, HookReply, Served};
use openxet_index::{GitStore as _, RepoId};

use common::{World, fixture_repo, ok};
use push_support::{
    Server, TEST_HOOK, blob, client_git, clone, commit, config, db_ref, engine, must, pack_of,
    raw_push, seeded, work,
};

const NODE: &str = "node";

fn set_mode(world: &World, mode: &str) {
    ok(std::fs::write(
        world.dir.path().join(NODE).join("test-hook-mode"),
        mode,
    ));
}

fn context() -> HookContext {
    HookContext {
        repo: RepoId::generate(),
        actor: "pusher".to_owned(),
        request: "req-1".to_owned(),
    }
}

/// What a `list-fds` child reported: its open descriptors by number and
/// kind, and the hook channel it was told about, if any.
#[derive(Debug, Default)]
struct Fds {
    open: BTreeMap<u32, String>,
    hook: Option<(u32, u32)>,
}

async fn child_fds(mut cmd: tokio::process::Command) -> Fds {
    let out = ok(cmd.stdin(Stdio::null()).output().await);
    let mut fds = Fds::default();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "hook" {
            let (r, w) = value.split_once(',').unwrap_or_default();
            fds.hook = Some((ok(r.parse()), ok(w.parse())));
        } else {
            fds.open.insert(ok(key.parse()), value.to_owned());
        }
    }
    fds
}

fn list_fds() -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(TEST_HOOK);
    cmd.arg("list-fds");
    cmd
}

fn ok_reply(name: &str) -> HookReply {
    HookReply {
        messages: Vec::new(),
        results: vec![(name.to_owned(), Ok(()))],
    }
}

/// Only the attached child holds the channel, as two pipes.
async fn channel_reaches_only_the_attached_child() {
    // Children compare by descriptor number against a sibling spawned
    // without the channel, before and at the same time as the attached
    // child: whatever this test process itself inherited without
    // `FD_CLOEXEC` (CI runners hand down pipes of their own) reaches every
    // child alike, and the channel's two descriptors must reach the
    // attached child only.
    let mut server = ok(HookServer::new(context()));
    let mut attached = list_fds();
    server.attach(&mut attached);
    let before = child_fds(list_fds()).await;
    let (attached, during) = tokio::join!(child_fds(attached), child_fds(list_fds()));
    let (read, write) = attached
        .hook
        .unwrap_or_else(|| panic!("the attached child is told the channel: {attached:?}"));
    assert_eq!(
        attached.open.get(&read).map(String::as_str),
        Some("fifo"),
        "{attached:?}"
    );
    assert_eq!(
        attached.open.get(&write).map(String::as_str),
        Some("fifo"),
        "{attached:?}"
    );
    for sibling in [&before, &during] {
        assert_eq!(sibling.hook, None);
        assert!(
            !sibling.open.contains_key(&read) && !sibling.open.contains_key(&write),
            "no other child inherits the channel: {sibling:?}"
        );
        let mut expected = sibling.open.clone();
        expected.insert(read, "fifo".to_owned());
        expected.insert(write, "fifo".to_owned());
        assert_eq!(
            attached.open, expected,
            "the channel is all the attached child gets extra"
        );
    }
    drop(server);
}

/// `hook_channel_private` (ADR 0004): the channel is two inherited pipes
/// with no path and no listener; only the receive-pack child gets them
/// without `FD_CLOEXEC`; a missing, wrong, reused or expired token is
/// rejected.
#[tokio::test(flavor = "multi_thread")]
async fn hook_channel_private() {
    channel_reaches_only_the_attached_child().await;

    // The right token is answered once.
    let mut server = ok(HookServer::new(context()));
    let mut cmd = tokio::process::Command::new(TEST_HOOK);
    cmd.arg("exchange");
    server.attach(&mut cmd);
    let child = ok(cmd.stdin(Stdio::null()).stdout(Stdio::piped()).spawn());
    drop(cmd);
    let outcome = server.serve(|ctx, request| async move {
        assert_eq!(ctx.actor, "pusher");
        assert_eq!(request.commands.len(), 1);
        Some(ok_reply("refs/heads/probe"))
    });
    let (outcome, out) = tokio::join!(outcome, child.wait_with_output());
    assert_eq!(outcome, Served::Answered);
    assert_eq!(
        String::from_utf8_lossy(&ok(out).stdout).trim(),
        "ok refs/heads/probe"
    );

    // A wrong token never reaches the handler.
    let mut server = ok(HookServer::new(context()));
    let mut cmd = tokio::process::Command::new(TEST_HOOK);
    cmd.arg("exchange");
    server.attach(&mut cmd);
    cmd.env(HOOK_TOKEN_ENV, "0".repeat(64));
    let child = ok(cmd.stdin(Stdio::null()).stdout(Stdio::piped()).spawn());
    drop(cmd);
    let outcome = server.serve(|_, _| async { panic!("the handler must not run") });
    let (outcome, out) = tokio::join!(outcome, child.wait_with_output());
    assert!(matches!(outcome, Served::Rejected(_)), "{outcome:?}");
    assert_eq!(
        String::from_utf8_lossy(&ok(out).stdout).trim(),
        "error the hook token is missing or wrong"
    );

    // Once the request has ended, the channel is gone: the token expired.
    let mut server = ok(HookServer::new(context()));
    let mut cmd = tokio::process::Command::new(TEST_HOOK);
    cmd.arg("exchange");
    server.attach(&mut cmd);
    drop(server);
    let out = ok(cmd.stdin(Stdio::null()).output().await);
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("error "),
        "{out:?}"
    );

    // Through a real push: a missing token fails the push; a replayed
    // request is refused while the first is answered.
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let node = engine(&world, config(&world, NODE)).await;
    let server = Server::start(vec![node]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let before = db_ref(&world, repo, "refs/heads/main").await;
    let tip = commit(&w, "one", &[("a.txt", b"a\n")]);

    set_mode(&world, "no-token");
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(!run.success);
    assert!(
        run.stderr.contains("the hook token is missing or wrong"),
        "{}",
        run.stderr
    );
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, before);

    set_mode(&world, &format!("token {}", "f".repeat(64)));
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(
        run.stderr.contains("the hook token is missing or wrong"),
        "{}",
        run.stderr
    );
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, before);

    let replay = world.dir.path().join("replay.txt");
    set_mode(&world, &format!("replay {}", replay.display()));
    must(&w, &["push", "-q", "origin", "main"]);
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip));
    assert_eq!(
        ok(std::fs::read_to_string(&replay)),
        "error the hook token was already used"
    );
}

/// `hook_cannot_choose_repo` (ADR 0004): lines naming another repository,
/// principal or request are ignored; the token's context wins.
#[tokio::test(flavor = "multi_thread")]
async fn hook_cannot_choose_repo() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let (other, _fx2) = seeded(&world, 10).await;
    let node = engine(&world, config(&world, NODE)).await;
    let server = Server::start(vec![node]).await;
    let other_before = ok(world.index.snapshot(other).await);
    set_mode(
        &world,
        &format!(
            "extra repo {other}\nextra principal admin\nextra actor admin\nextra request forged"
        ),
    );
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let tip = commit(&w, "one", &[("a.txt", b"a\n")]);
    must(&w, &["push", "-q", "origin", "main"]);

    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(tip));
    let other_after = ok(world.index.snapshot(other).await);
    assert_eq!(other_after.refs, other_before.refs);
    assert_eq!(other_after.packs, other_before.packs);
    let log = ok(world.index.ref_log_since(repo, 0).await);
    let row = log.last().unwrap_or_else(|| panic!("logged"));
    assert_eq!(row.actor, "pusher");
    assert_ne!(row.request, "forged");
    assert!(
        ok(world.index.ref_log_since(other, 0).await)
            .iter()
            .all(|r| r.actor != "admin")
    );
}

/// `hook_pack_path_confined` (ADR 0004): a reported pack path outside the
/// request's snapshot is refused, including symlink and `..` escapes; the
/// parent finds the push's pack itself, so a hook that reports none
/// changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn hook_pack_path_confined() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let node = engine(&world, config(&world, NODE)).await;
    let server = Server::start(vec![node]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let before = ok(world.index.snapshot(repo).await);
    commit(&w, "one", &[("a.txt", b"a\n")]);

    let outside = world.dir.path().join("outside");
    ok(std::fs::create_dir_all(&outside));
    let name = format!("pack-{}", "1f".repeat(20));
    for ext in ["pack", "idx", "keep"] {
        ok(std::fs::write(
            outside.join(format!("{name}.{ext}")),
            b"not a pack",
        ));
    }
    let outside_pack = outside.join(format!("{name}.pack"));
    let escapes = [
        format!("pack {}", outside_pack.display()),
        format!("pack ./../../../outside/{name}.pack"),
        format!(
            "symlink {name}.pack {}\npack ./{name}.pack",
            outside_pack.display()
        ),
        format!("pack /etc/{name}.pack"),
    ];
    for mode in escapes {
        set_mode(&world, &mode);
        let run = client_git(&w, &["push", "origin", "main"]);
        assert!(!run.success, "{mode}");
        assert!(
            run.stderr
                .contains("the hook reported a pack outside this push"),
            "{mode}: {}",
            run.stderr
        );
        let after = ok(world.index.snapshot(repo).await);
        assert_eq!(after.refs, before.refs, "{mode}");
        assert_eq!(after.packs, before.packs, "{mode}");
    }

    set_mode(&world, "no-pack");
    let tip = must(&w, &["rev-parse", "HEAD"]);
    must(&w, &["push", "-q", "origin", "main"]);
    assert_eq!(
        db_ref(&world, repo, "refs/heads/main")
            .await
            .map(|o| o.to_string()),
        Some(tip.trim().to_owned())
    );
}

/// `hook_policy_rechecked` (ADR 0004): openxet refuses a deletion of
/// `main`, an update whose old value isn't the ref's (as a forged relay
/// would claim), and a non-commit on a branch, whatever git and the hook
/// let through. A forced update is a non-fast-forward on the wire that only
/// the client can tell apart, and is accepted.
#[tokio::test(flavor = "multi_thread")]
async fn hook_policy_rechecked() {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, 10).await;
    let node = engine(&world, config(&world, NODE)).await;
    let server = Server::start(vec![node.clone()]).await;
    let client = clone(&fixture_repo(&fx), &server.url(0, repo), &[]);
    let w = work(&client);
    let main = db_ref(&world, repo, "refs/heads/main").await;

    let run = client_git(&w, &["push", "origin", ":main"]);
    assert!(!run.success);
    assert!(
        run.stderr
            .contains("deletion of the current branch prohibited"),
        "{}",
        run.stderr
    );
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, main);

    commit(&w, "one", &[("a.txt", b"a\n")]);
    set_mode(
        &world,
        &format!("old {}", must(&w, &["rev-parse", "HEAD~3"]).trim()),
    );
    let run = client_git(&w, &["push", "origin", "main"]);
    assert!(!run.success);
    assert!(run.stderr.contains("fetch first"), "{}", run.stderr);
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, main);
    set_mode(&world, "");

    let b = blob(&w, b"not a commit\n");
    let r = raw_push(
        &node,
        repo,
        "pusher",
        &[(None, Some(b), "refs/heads/blobby")],
        Some(pack_of(&w, &[b])),
    )
    .await;
    assert_eq!(
        r.ng("refs/heads/blobby").as_deref(),
        Some("trying to write non-commit object to branch"),
        "{r:?}"
    );

    must(&w, &["reset", "-q", "--hard", "HEAD~2"]);
    let forced = commit(&w, "rewritten", &[("b.txt", b"b\n")]);
    must(&w, &["push", "-q", "--force", "origin", "main"]);
    assert_eq!(db_ref(&world, repo, "refs/heads/main").await, Some(forced));
}
