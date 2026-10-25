//! The git process (ADR 0004 items 1, 7 and 8): the version floor, the
//! sanitised environment, the pinned snapshot config and the captured v2
//! advertisement.
//!
//! The parent's own environment can't be changed from a test (`set_var` is
//! `unsafe`, which the workspace forbids), so the tests that need a
//! poisoned `PATH` or `HOME` rerun one ignored test of this binary in a
//! child process with that environment.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use common::{World, ok};
use openxet_git::{Engine, GitError, PINNED_CONFIG, git_command, protocol_version};

const CHILD: &str = "OPENXET_GIT_TEST_CHILD";

/// Runs the ignored test `name` of this binary in a child with `env` added
/// and asserts it passed.
fn run_child(name: &str, env: &[(&str, OsString)]) {
    let out = ok(Command::new(ok(std::env::current_exe()))
        .args([name, "--exact", "--include-ignored", "--nocapture"])
        .env(CHILD, "1")
        .envs(env.iter().map(|(k, v)| (*k, v)))
        .output());
    let report = format!(
        "child {name} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{report}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "{report}"
    );
}

fn is_child() -> bool {
    std::env::var_os(CHILD).is_some()
}

#[test]
fn git_version_floor() {
    let bin = ok(tempfile::TempDir::new());
    let fake = bin.path().join("git");
    ok(fs::write(&fake, "#!/bin/sh\necho 'git version 2.36.0'\n"));
    ok(fs::set_permissions(
        &fake,
        fs::Permissions::from_mode(0o755),
    ));
    run_child(
        "child_git_version_floor",
        &[("PATH", bin.path().as_os_str().to_owned())],
    );
}

#[tokio::test]
#[ignore = "run by git_version_floor in a child process"]
async fn child_git_version_floor() {
    if !is_child() {
        return;
    }
    let world = World::new().await;
    let err = Engine::start(world.index.clone(), world.bucket.clone(), world.config())
        .await
        .err()
        .unwrap_or_else(|| panic!("start accepted git 2.36.0"));
    assert!(matches!(err, GitError::GitVersion { .. }), "{err:?}");
    let msg = err.to_string();
    assert!(
        msg.contains("2.37") && msg.contains("2.36.0") && msg.contains("PATH"),
        "{msg}"
    );
}

#[test]
fn git_env_sanitised() {
    let home = ok(tempfile::TempDir::new());
    ok(fs::write(
        home.path().join(".gitconfig"),
        "[uploadpack]\n\tallowAnySHA1InWant = true\n[core]\n\thooksPath = /evil\n",
    ));
    ok(fs::create_dir_all(home.path().join(".config/git")));
    ok(fs::write(
        home.path().join(".config/git/config"),
        "[uploadpack]\n\tallowFilter = true\n",
    ));
    let path = std::env::var_os("PATH").unwrap_or_default();
    run_child(
        "child_git_env_sanitised",
        &[
            ("PATH", path),
            ("HOME", home.path().as_os_str().to_owned()),
            (
                "XDG_CONFIG_HOME",
                home.path().join(".config").into_os_string(),
            ),
            ("GIT_PROTOCOL", "version=2;evil".into()),
            ("GIT_DIR", "/nonexistent".into()),
            (
                "GIT_CONFIG_PARAMETERS",
                "'uploadpack.allowanysha1inwant'='true'".into(),
            ),
            ("GIT_CONFIG_COUNT", "1".into()),
            ("GIT_CONFIG_KEY_0", "uploadpack.allowfilter".into()),
            ("GIT_CONFIG_VALUE_0", "true".into()),
        ],
    );
}

fn env_of(out: &std::process::Output) -> BTreeMap<String, String> {
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
}

#[tokio::test]
#[ignore = "run by git_env_sanitised in a child process"]
async fn child_git_env_sanitised() {
    if !is_child() {
        return;
    }
    let world = World::new().await;
    let repo = world.repo("org", "model").await;
    let engine = world.engine().await;
    let snap = ok(engine.snapshot(repo).await);
    let cache = ok(fs::canonicalize(world.cache_dir()));
    let env_bin = Path::new("/usr/bin/env");

    let out = ok(git_command(env_bin, &snap, Some(2)).output().await);
    let env = env_of(&out);
    let keys: Vec<&str> = env.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_NOSYSTEM",
            "GIT_PROTOCOL",
            "HOME",
            "PATH"
        ],
        "{env:?}"
    );
    assert_eq!(env["GIT_PROTOCOL"], "version=2");
    assert_eq!(env["GIT_CONFIG_NOSYSTEM"], "1");
    assert_eq!(env["GIT_CONFIG_GLOBAL"], "/dev/null");
    let home = Path::new(&env["HOME"]);
    assert!(home.starts_with(&cache), "{home:?}");
    assert_eq!(ok(fs::read_dir(home)).count(), 0, "HOME is empty");
    let exec_path = common::git(Path::new("/"), &["--exec-path"]);
    let path: Vec<&str> = env["PATH"].split(':').collect();
    assert_eq!(path[0], exec_path.trim());
    assert!(
        path.contains(&cache.join("hooks").to_string_lossy().as_ref()),
        "{path:?}"
    );

    // Nothing but a validated version reaches GIT_PROTOCOL.
    for protocol in [None, Some(3), Some(255)] {
        let out = ok(git_command(env_bin, &snap, protocol).output().await);
        assert!(!env_of(&out).contains_key("GIT_PROTOCOL"), "{protocol:?}");
    }

    // The caller's HOME, XDG and GIT_CONFIG_* settings change nothing: git
    // reads the snapshot's config and nothing else.
    let out = ok(git_command(engine.git(), &snap, Some(2))
        .args(["config", "--list", "--show-origin"])
        .output()
        .await);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);
    for line in listing.lines() {
        assert!(line.starts_with("file:config\t"), "{line}");
    }
    assert!(
        listing.contains("uploadpack.allowanysha1inwant=false"),
        "{listing}"
    );
    assert!(
        listing.contains("uploadpack.allowfilter=false"),
        "{listing}"
    );
    assert!(!listing.contains("/evil"), "{listing}");
}

#[test]
fn protocol_header_validated() {
    assert_eq!(protocol_version(b"version=0"), Some(0));
    assert_eq!(protocol_version(b"version=1"), Some(1));
    assert_eq!(protocol_version(b"version=2"), Some(2));
    for bad in [
        &b"version=2;evil"[..],
        b"version=2:object-format=sha1\n",
        b"version=3",
        b"version=22",
        b" version=2",
        b"VERSION=2",
        b"",
    ] {
        assert_eq!(
            protocol_version(bad),
            None,
            "{:?}",
            String::from_utf8_lossy(bad)
        );
    }
}

#[tokio::test]
async fn upload_pack_config_pinned() {
    let world = World::new().await;
    let repo = world.repo("org", "model").await;
    let engine = world.engine().await;
    let snap = ok(engine.snapshot(repo).await);

    let out = ok(git_command(engine.git(), &snap, None)
        .args(["config", "--list"])
        .output()
        .await);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listing = String::from_utf8_lossy(&out.stdout);
    let config: BTreeMap<&str, &str> = listing.lines().filter_map(|l| l.split_once('=')).collect();
    for (key, value) in PINNED_CONFIG {
        assert_eq!(
            config.get(key.to_ascii_lowercase().as_str()),
            Some(value),
            "{key} in {listing}"
        );
    }
    let hooks = ok(fs::canonicalize(world.cache_dir())).join("hooks");
    assert_eq!(
        config.get("core.hookspath"),
        Some(&hooks.to_string_lossy().as_ref())
    );
    assert!(hooks.is_dir() && ok(fs::read_dir(&hooks)).count() == 0);
    assert_eq!(config.get("core.bare"), Some(&"true"));
}

#[tokio::test]
async fn v2_advertisement_captured() {
    let world = World::new().await;
    let engine = world.engine().await;
    let adv = String::from_utf8_lossy(engine.v2_advertisement()).into_owned();
    assert!(adv.starts_with("000eversion 2\n"), "{adv:?}");
    assert!(adv.contains("ls-refs") && adv.contains("fetch"), "{adv:?}");
    assert!(adv.ends_with("0000"), "{adv:?}");
}
