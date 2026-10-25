//! The `openxet` binary is the `proc-receive` hook (ADR 0004 item 3): run
//! through a symlink named `proc-receive`, as git runs it from the Git
//! cache's hook directory, it relays a real push to the engine; outside a
//! push it fails with an actionable message.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use bytes::Bytes;
use futures_util::StreamExt as _;
use openxet_auth::UserPrincipal;
use openxet_git::{Engine, GitConfig};
use openxet_index::{GitStore as _, Repo, RepoId, Repos as _, SqliteIndex};
use openxet_storage::LocalBucket;

const OPENXET: &str = env!("CARGO_BIN_EXE_openxet");

fn git(dir: &Path, args: &[&str]) -> Vec<u8> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@openxet.invalid"])
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn hook_outside_a_push_names_what_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("proc-receive");
    std::os::unix::fs::symlink(OPENXET, &link).unwrap();
    for mut cmd in [Command::new(&link), {
        let mut c = Command::new(OPENXET);
        c.arg("proc-receive");
        c
    }] {
        let out = cmd.env_remove("OPENXET_HOOK_FD").output().unwrap();
        assert_eq!(out.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("OPENXET_HOOK_FD is not set"), "{stderr}");
    }
    let help = Command::new(OPENXET).arg("--help").output().unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("proc-receive"));
}

#[tokio::test(flavor = "multi_thread")]
async fn openxet_relays_a_push_as_the_hook() {
    let dir = tempfile::tempdir().unwrap();
    let index = Arc::new(
        SqliteIndex::open(&dir.path().join("index.sqlite"))
            .await
            .unwrap(),
    );
    let bucket = Arc::new(LocalBucket::open(&dir.path().join("bucket")).unwrap());
    let repo = Repo {
        id: RepoId::generate(),
        namespace: "org".to_owned(),
        name: "model".to_owned(),
        private: false,
        labels: vec!["model".to_owned()],
        created_at: 1_761_400_000,
        full_repack_gen: 0,
    };
    index.create_repo(&repo).await.unwrap();
    let mut config = GitConfig::new(dir.path().join("git-cache"));
    config.hook_program = Some(OPENXET.into());
    let engine = Engine::start(index.clone(), bucket, config).await.unwrap();

    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q"]);
    std::fs::write(work.join("README.md"), "hello\n").unwrap();
    git(&work, &["add", "README.md"]);
    git(&work, &["commit", "-q", "-m", "first"]);
    let tip = String::from_utf8(git(&work, &["rev-parse", "HEAD"])).unwrap();
    let tip = tip.trim();
    let pack = {
        use std::io::Write as _;
        let mut child = Command::new("git")
            .current_dir(&work)
            .args(["pack-objects", "-q", "--revs", "--stdout"])
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"HEAD\n").unwrap();
        child.wait_with_output().unwrap().stdout
    };
    let line = format!(
        "{} {tip} refs/heads/main\0report-status object-format=sha1\n",
        "0".repeat(40)
    );
    let mut request = format!("{:04x}", line.len() + 4).into_bytes();
    request.extend_from_slice(line.as_bytes());
    request.extend_from_slice(b"0000");
    request.extend_from_slice(&pack);
    let body: openxet_storage::BodyStream =
        Box::pin(futures_util::stream::iter([Ok(Bytes::from(request))]));
    let who = UserPrincipal {
        subject: "alice".to_owned(),
        name: "alice".to_owned(),
        email: None,
        claims: std::collections::BTreeMap::default(),
    };
    let mut response = engine.receive_pack(repo.id, 0, &who, body).await.unwrap();
    let mut out = Vec::new();
    while let Some(chunk) = response.next().await {
        out.extend_from_slice(&chunk.unwrap());
    }
    let out = String::from_utf8_lossy(&out);
    assert!(out.contains("ok refs/heads/main"), "{out}");
    let snap = index.snapshot(repo.id).await.unwrap();
    assert_eq!(snap.refs.len(), 1);
    assert_eq!(snap.refs[0].1.to_string(), tip);
    let log = index.ref_log_since(repo.id, 0).await.unwrap();
    assert_eq!(log[0].actor, "alice");
}
