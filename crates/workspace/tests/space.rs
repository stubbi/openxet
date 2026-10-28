#![allow(clippy::unwrap_used)]

mod support;

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use openxet_workspace::{
    Budget, ProjectId, Registry, Volume, Workspace, WorkspaceError, WorkspaceId, evict_expired,
    eviction_order, free_to_floor,
};
use support::{Repo, git};

const GIB: u64 = 1 << 30;

fn workspace(path: &Path, last_active: SystemTime) -> Workspace {
    Workspace {
        id: WorkspaceId::generate(),
        project: ProjectId::discover(path).unwrap(),
        path: path.to_path_buf(),
        branch: None,
        created_at: last_active,
        last_active,
        adopted: false,
    }
}

fn hours_ago(hours: u64) -> SystemTime {
    SystemTime::now() - Duration::from_hours(hours)
}

/// A worktree of `repo` with Cargo-like build outputs, registered in `reg`
/// as last active at `last_active`.
async fn built_workspace(
    repo: &Repo,
    reg: &Registry,
    name: &str,
    last_active: SystemTime,
) -> Workspace {
    let path = repo.worktree(name, "main");
    fs::create_dir_all(path.join("target/debug/deps")).unwrap();
    fs::create_dir_all(path.join("target/debug/incremental/lib-1")).unwrap();
    fs::write(path.join("target/debug/deps/lib.rlib"), [1u8; 4096]).unwrap();
    fs::write(
        path.join("target/debug/incremental/lib-1/q.bin"),
        [2u8; 4096],
    )
    .unwrap();
    // What Cargo writes at the top of every target directory it creates.
    fs::write(
        path.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n\
         # This file is a cache directory tag created by cargo.\n",
    )
    .unwrap();
    let ws = workspace(&path, last_active);
    reg.insert(&ws).await.unwrap();
    ws
}

async fn registry() -> (tempfile::TempDir, Registry) {
    let state = tempfile::tempdir().unwrap();
    let reg = Registry::open(state.path()).await.unwrap();
    (state, reg)
}

fn volume(free_bytes: u64) -> Volume {
    Volume {
        free_bytes,
        total_bytes: 100 * GIB,
    }
}

#[allow(clippy::unnecessary_wraps)] // The signature `free_to_floor` takes.
fn none_active(_: &[Workspace]) -> Option<HashSet<WorkspaceId>> {
    Some(HashSet::new())
}

#[test]
fn floor_defaults_to_twenty_gib_or_ten_percent() {
    let budget = Budget::default();
    assert_eq!(budget.floor_for(volume(0)), 20 * GIB);
    let big = Volume {
        free_bytes: 0,
        total_bytes: 1 << 40,
    };
    assert_eq!(budget.floor_for(big), (1 << 40) / 10);
    assert_eq!(budget.idle_ttl, Duration::from_hours(24));
    assert_eq!(budget.settle, Duration::from_secs(2));
}

#[test]
fn config_overrides_floor_and_idle_ttl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workspace.toml");
    assert_eq!(Budget::load(&path).unwrap(), Budget::default());

    fs::write(&path, "floor = \"50 GiB\"\nidle_ttl = \"90m\"\n").unwrap();
    let budget = Budget::load(&path).unwrap();
    assert_eq!(budget.floor_for(volume(0)), 50 * GIB);
    assert_eq!(budget.idle_ttl, Duration::from_mins(90));

    fs::write(&path, "floor = 1048576\n").unwrap();
    assert_eq!(Budget::load(&path).unwrap().floor_for(volume(0)), 1 << 20);

    for bad in [
        "floor = \"50 GB\"\n",
        "idle_ttl = \"soon\"\n",
        "idle_ttl = \"0h\"\n",
        "ttl = \"1h\"\n",
    ] {
        fs::write(&path, bad).unwrap();
        let err = Budget::load(&path).unwrap_err();
        assert!(
            matches!(err, WorkspaceError::Config { .. }),
            "{bad:?} gave {err:?}"
        );
    }
}

#[test]
fn eviction_skips_active_workspaces() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet"]);
    let now = SystemTime::now();
    let busy = workspace(dir.path(), hours_ago(5));
    let recent = workspace(dir.path(), now - Duration::from_mins(2));
    let idle = workspace(dir.path(), hours_ago(1));
    let active = HashSet::from([busy.id]);

    let order = eviction_order(&[busy, recent, idle.clone()], now, &active);

    assert_eq!(order, vec![idle.id]);
}

#[test]
fn eviction_orders_least_recently_active_first() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet"]);
    let middle = workspace(dir.path(), hours_ago(2));
    let newest = workspace(dir.path(), hours_ago(1));
    let oldest = workspace(dir.path(), hours_ago(3));

    let order = eviction_order(
        &[middle.clone(), newest.clone(), oldest.clone()],
        SystemTime::now(),
        &HashSet::new(),
    );

    assert_eq!(order, vec![oldest.id, middle.id, newest.id]);
}

#[tokio::test]
async fn eviction_removes_only_reconstructible_dirs() {
    let repo = Repo::new();
    let (_state, reg) = registry().await;
    let ws = built_workspace(&repo, &reg, "one", hours_ago(2)).await;
    let root = &ws.path;
    // Worktrees share the main repository's `info/exclude`. `results/` is
    // ignored but named by no preset: results an agent produced.
    fs::write(repo.main.join(".git/info/exclude"), "/results/\n/.venv/\n").unwrap();
    fs::create_dir_all(root.join("results")).unwrap();
    fs::write(root.join("results/run.csv"), "a,b\n").unwrap();
    fs::write(root.join("notes.txt"), "untracked\n").unwrap();
    fs::write(root.join("lib.rs"), "fn main() { edited() }\n").unwrap();
    // `.venv` is a preset and ignored, but holds a tracked file, so it is
    // durable as a whole.
    fs::create_dir_all(root.join(".venv")).unwrap();
    fs::write(root.join(".venv/pinned.txt"), "tracked\n").unwrap();
    git(root, &["add", "--force", ".venv/pinned.txt"]);
    let canonical = root.join("target/debug/deps/lib.rlib");
    reg.set_canonical(4096, [1; 32], &canonical).await.unwrap();
    let durable_canonical = root.join("results/run.csv");
    reg.set_canonical(4, [2; 32], &durable_canonical)
        .await
        .unwrap();

    // Free space never recovers, so every stage runs.
    let evicted = free_to_floor(
        &reg,
        &Budget::default(),
        SystemTime::now(),
        none_active,
        |_| Ok(volume(GIB)),
    )
    .await
    .unwrap();

    assert!(evicted.contains(&root.join("target")), "{evicted:?}");
    assert!(!root.join("target").exists());
    assert_eq!(
        fs::read_to_string(root.join("results/run.csv")).unwrap(),
        "a,b\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("notes.txt")).unwrap(),
        "untracked\n"
    );
    assert_eq!(
        fs::read_to_string(root.join("lib.rs")).unwrap(),
        "fn main() { edited() }\n"
    );
    assert_eq!(
        fs::read_to_string(root.join(".venv/pinned.txt")).unwrap(),
        "tracked\n"
    );
    assert_eq!(reg.canonical(4096, [1; 32]).await.unwrap(), None);
    assert_eq!(
        reg.canonical(4, [2; 32]).await.unwrap(),
        Some(durable_canonical)
    );
}

#[tokio::test]
async fn eviction_takes_cargo_incremental_dirs_first() {
    let repo = Repo::new();
    let (_state, reg) = registry().await;
    let ws = built_workspace(&repo, &reg, "one", hours_ago(2)).await;
    let incremental = ws.path.join("target/debug/incremental");
    // Back over the floor as soon as `incremental/` is gone.
    let free = |_: &Path| Ok(volume(if incremental.exists() { GIB } else { 90 * GIB }));

    let evicted = free_to_floor(
        &reg,
        &Budget::default(),
        SystemTime::now(),
        none_active,
        free,
    )
    .await
    .unwrap();

    assert_eq!(evicted, vec![incremental.clone()]);
    assert!(!incremental.exists());
    assert!(ws.path.join("target/debug/deps/lib.rlib").exists());
}

#[tokio::test]
async fn eviction_waits_while_activity_is_unknown() {
    let repo = Repo::new();
    let (_state, reg) = registry().await;
    let ws = built_workspace(&repo, &reg, "one", hours_ago(2)).await;

    let evicted = free_to_floor(
        &reg,
        &Budget::default(),
        SystemTime::now(),
        |_| None,
        |_| Ok(volume(GIB)),
    )
    .await
    .unwrap();

    assert!(evicted.is_empty());
    assert!(ws.path.join("target/debug/deps/lib.rlib").exists());
}

#[tokio::test]
async fn ttl_evicts_idle_reconstructible_dirs() {
    let repo = Repo::new();
    let (_state, reg) = registry().await;
    let stale = built_workspace(&repo, &reg, "stale", hours_ago(3)).await;
    let fresh = built_workspace(&repo, &reg, "fresh", hours_ago(1)).await;
    let busy = built_workspace(&repo, &reg, "busy", hours_ago(5)).await;
    let budget = Budget {
        idle_ttl: Duration::from_hours(2),
        ..Budget::default()
    };
    let active = HashSet::from([busy.id]);

    let evicted = evict_expired(&reg, &budget, SystemTime::now(), &active)
        .await
        .unwrap();

    assert_eq!(evicted, vec![stale.path.join("target")]);
    assert!(!stale.path.join("target").exists());
    assert!(stale.path.join("lib.rs").exists());
    assert!(fresh.path.join("target/debug/deps/lib.rlib").exists());
    assert!(busy.path.join("target/debug/deps/lib.rlib").exists());
}
