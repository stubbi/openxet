#![allow(clippy::unwrap_used)]

mod support;

use std::fs::{self, File, FileTimes};
use std::io;
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::Path;
use std::time::{Duration, SystemTime};

use openxet_workspace::{
    Presets, ProjectId, Registry, SeedReport, SeedSkip, Workspace, WorkspaceId, nearest_sibling,
    reflink_supported, seed, seed_with, wait_until_settled,
};
use support::{Repo, git};

fn set_mtime(path: &Path, at: SystemTime) {
    File::open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(at))
        .unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    fs::symlink_metadata(path).unwrap().modified().unwrap()
}

/// Seeds with real clones where the temp dir's filesystem has them, and
/// through a byte-copying stand-in elsewhere, so the metadata handling is
/// tested on every CI filesystem.
fn seed_here(root: &Path, target: &Path, from: &Path) -> SeedReport {
    if reflink_supported(root) {
        seed(target, Some(from), &Presets::default())
    } else {
        seed_with(target, Some(from), &Presets::default(), &|from, to| {
            fs::copy(from, to).map(drop)
        })
    }
}

#[test]
fn seeded_target_keeps_mtimes() {
    let repo = Repo::new();
    let from = repo.worktree("from", "main");
    let hour_ago = SystemTime::now() - Duration::from_hours(1);
    fs::create_dir_all(from.join("target/debug/deps")).unwrap();
    let rlib = "target/debug/deps/libx.rlib";
    fs::write(from.join(rlib), vec![7u8; 10_000]).unwrap();
    let script = "target/debug/build-script";
    fs::write(from.join(script), "#!/bin/sh\n").unwrap();
    fs::set_permissions(from.join(script), fs::Permissions::from_mode(0o750)).unwrap();
    symlink("deps/libx.rlib", from.join("target/debug/libx.rlib")).unwrap();
    set_mtime(&from.join(rlib), hour_ago);
    set_mtime(&from.join(script), hour_ago - Duration::from_mins(1));
    // A file a build may still be writing: not settled, so not cloned.
    fs::write(from.join("target/debug/writing"), "half").unwrap();
    // Cargo compares sources' mtimes with its outputs': a checkout's fresh
    // mtimes would make every seeded crate of the workspace dirty.
    set_mtime(&from.join("lib.rs"), hour_ago - Duration::from_hours(1));
    let target = repo.worktree("target-ws", "main");

    let report = seed_here(&repo.root, &target, &from);

    assert_eq!(report.skipped, None);
    assert_eq!(report.cloned_bytes, 10_000 + 10);
    assert_eq!(fs::read(target.join(rlib)).unwrap(), vec![7u8; 10_000]);
    assert_eq!(mtime(&target.join(rlib)), hour_ago);
    assert_eq!(
        mtime(&target.join(script)),
        hour_ago - Duration::from_mins(1)
    );
    let mode = fs::metadata(target.join(script))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o7777, 0o750);
    assert_eq!(
        fs::read_link(target.join("target/debug/libx.rlib")).unwrap(),
        Path::new("deps/libx.rlib")
    );
    assert!(!target.join("target/debug/writing").exists());
    assert_eq!(mtime(&target.join("lib.rs")), mtime(&from.join("lib.rs")));
    assert_eq!(git(&target, &["status", "--porcelain"]), "");
}

#[test]
fn a_source_file_that_differs_keeps_its_own_mtime() {
    let repo = Repo::new();
    let from = repo.worktree("from", "main");
    fs::create_dir_all(from.join("target")).unwrap();
    fs::write(from.join("target/out"), "x").unwrap();
    let old = SystemTime::now() - Duration::from_hours(1);
    set_mtime(&from.join("target/out"), old);
    fs::write(from.join("lib.rs"), "fn main() { edited() }\n").unwrap();
    set_mtime(&from.join("lib.rs"), old);
    let target = repo.worktree("target-ws", "main");
    let checked_out = mtime(&target.join("lib.rs"));

    assert_eq!(seed_here(&repo.root, &target, &from).skipped, None);
    assert_eq!(mtime(&target.join("lib.rs")), checked_out);
}

#[test]
fn seed_without_reflink_support_skips() {
    let repo = Repo::new();
    let from = repo.worktree("from", "main");
    fs::create_dir_all(from.join("target/debug")).unwrap();
    fs::write(from.join("target/debug/out"), vec![1u8; 4096]).unwrap();
    set_mtime(
        &from.join("target/debug/out"),
        SystemTime::now() - Duration::from_hours(1),
    );
    let target = repo.worktree("target-ws", "main");

    let unsupported =
        |_: &Path, _: &Path| -> io::Result<()> { Err(io::Error::from(io::ErrorKind::Unsupported)) };
    let report = seed_with(&target, Some(&from), &Presets::default(), &unsupported);
    assert_eq!(report.skipped, Some(SeedSkip::ReflinkUnsupported));
    assert_eq!(report.cloned_bytes, 0);
    assert!(!target.join("target").exists(), "never a byte copy");

    if !reflink_supported(&repo.root) {
        let report = seed(&target, Some(&from), &Presets::default());
        assert_eq!(report.skipped, Some(SeedSkip::ReflinkUnsupported));
        assert!(!target.join("target").exists(), "never a byte copy");
    }
}

#[test]
fn seed_without_a_sibling_skips() {
    let repo = Repo::new();
    let target = repo.worktree("target-ws", "main");
    let report = seed(&target, None, &Presets::default());
    assert_eq!(report.skipped, Some(SeedSkip::NoSibling));
    assert_eq!(report.cloned_bytes, 0);
}

fn register(project: &ProjectId, path: &Path, last_active: SystemTime) -> Workspace {
    Workspace {
        id: WorkspaceId::generate(),
        project: project.clone(),
        path: path.to_path_buf(),
        branch: None,
        created_at: last_active,
        last_active,
        adopted: false,
    }
}

fn with_target(dir: &Path) {
    fs::create_dir_all(dir.join("target")).unwrap();
    fs::write(dir.join("target/out"), "x").unwrap();
}

#[tokio::test]
async fn seed_picks_the_nearest_sibling() {
    let repo = Repo::new();
    let state = tempfile::tempdir().unwrap();
    let registry = Registry::open(state.path()).await.unwrap();
    let project = ProjectId::discover(&repo.main).unwrap();
    let base = git(&repo.main, &["rev-parse", "HEAD"]);
    let now = SystemTime::now();

    let far = repo.worktree("far", "main");
    Repo::commit(&far, "a.rs", "a");
    Repo::commit(&far, "b.rs", "b");
    let near = repo.worktree("near", "main");
    let also_near = repo.worktree("also-near", "main");
    let unbuilt = repo.worktree("unbuilt", "main");
    for dir in [&far, &near, &also_near] {
        with_target(dir);
    }
    let target = repo.worktree("target-ws", "main");
    for (dir, active) in [
        (&far, now),
        (&near, now - Duration::from_hours(2)),
        (&also_near, now - Duration::from_hours(1)),
        (&unbuilt, now),
        (&target, now),
    ] {
        registry
            .insert(&register(&project, dir, active))
            .await
            .unwrap();
    }
    with_target(&target);

    let found = nearest_sibling(&registry, &project, &target, &base)
        .await
        .unwrap();
    assert_eq!(found, Some(also_near.clone()), "ties go to the most recent");

    fs::remove_dir_all(also_near.join("target")).unwrap();
    let found = nearest_sibling(&registry, &project, &target, &base)
        .await
        .unwrap();
    assert_eq!(
        found,
        Some(near),
        "a sibling with no build outputs is passed over"
    );
}

#[tokio::test]
async fn the_main_worktree_is_a_sibling_too() {
    let repo = Repo::new();
    let state = tempfile::tempdir().unwrap();
    let registry = Registry::open(state.path()).await.unwrap();
    let project = ProjectId::discover(&repo.main).unwrap();
    let base = git(&repo.main, &["rev-parse", "HEAD"]);
    let target = repo.worktree("target-ws", "main");

    let found = nearest_sibling(&registry, &project, &target, &base)
        .await
        .unwrap();
    assert_eq!(found, None);

    with_target(&repo.main);
    let found = nearest_sibling(&registry, &project, &target, &base)
        .await
        .unwrap();
    assert_eq!(found, Some(repo.main.clone()));
}

/// A sibling whose `target/` was last written an hour ago, with Cargo's
/// lock file for the `debug` profile.
fn settled_sibling(repo: &Repo) -> std::path::PathBuf {
    let from = repo.worktree("from", "main");
    let hour_ago = SystemTime::now() - Duration::from_hours(1);
    fs::create_dir_all(from.join("target/debug/deps")).unwrap();
    for file in [
        "target/debug/deps/libx.rlib",
        "target/debug/.cargo-lock",
        "target/debug/.cargo-build-lock",
    ] {
        fs::write(from.join(file), "x").unwrap();
        set_mtime(&from.join(file), hour_ago);
    }
    from
}

#[test]
fn a_settled_sibling_is_seeded_without_waiting() {
    let repo = Repo::new();
    let from = settled_sibling(&repo);
    let wait = wait_until_settled(&from, &Presets::default(), Duration::from_secs(10));
    assert!(wait.settled);
    assert!(wait.waited < Duration::from_secs(1), "{:?}", wait.waited);
}

#[test]
fn a_file_just_written_is_waited_out() {
    let repo = Repo::new();
    let from = settled_sibling(&repo);
    fs::write(from.join("target/debug/deps/liby.rlib"), "just linked").unwrap();
    let wait = wait_until_settled(&from, &Presets::default(), Duration::from_secs(10));
    assert!(wait.settled);
    assert!(
        wait.waited >= Duration::from_millis(1500),
        "{:?}",
        wait.waited
    );
}

#[test]
fn a_held_cargo_lock_is_unsettled_until_released() {
    let repo = Repo::new();
    let from = settled_sibling(&repo);
    let lock = File::open(from.join("target/debug/.cargo-build-lock")).unwrap();
    lock.lock().unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        drop(lock);
    });
    let wait = wait_until_settled(&from, &Presets::default(), Duration::from_secs(10));
    release.join().unwrap();
    assert!(wait.settled);
    assert!(
        wait.waited >= Duration::from_millis(900),
        "{:?}",
        wait.waited
    );
}

#[test]
fn the_wait_stops_at_the_cap() {
    let repo = Repo::new();
    let from = settled_sibling(&repo);
    let lock = File::open(from.join("target/debug/.cargo-lock")).unwrap();
    lock.lock().unwrap();
    let cap = Duration::from_millis(500);
    let wait = wait_until_settled(&from, &Presets::default(), cap);
    assert!(!wait.settled);
    assert!(
        wait.waited >= cap && wait.waited < Duration::from_secs(3),
        "{:?}",
        wait.waited
    );
    drop(lock);
}
