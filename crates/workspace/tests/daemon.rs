#![allow(clippy::unwrap_used)]

mod support;

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_workspace::{
    Budget, ProjectId, Registry, Workspace, WorkspaceId, daemon_pid, daemon_running, run_daemon,
    stop_daemon,
};
use support::Repo;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// No floor, so a test daemon never evicts, and a short settle window.
fn budget() -> Budget {
    Budget {
        floor_bytes: Some(0),
        settle: Duration::from_millis(200),
        ..Budget::default()
    }
}

/// A daemon on `state`, stopped when the returned sender is dropped or
/// sent to, so no test leaves one running.
fn start(
    state: &Path,
) -> (
    oneshot::Sender<()>,
    JoinHandle<Result<(), openxet_workspace::WorkspaceError>>,
) {
    let (stop, stopped) = oneshot::channel::<()>();
    let state = state.to_path_buf();
    let handle = tokio::spawn(async move {
        run_daemon(&state, budget(), async {
            let _ = stopped.await;
        })
        .await
    });
    (stop, handle)
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn second_daemon_exits_when_lock_is_held() {
    let state = tempfile::tempdir().unwrap();
    assert!(!daemon_running(state.path()));
    let (stop, first) = start(state.path());
    wait_for("the first daemon to start", || daemon_running(state.path())).await;

    let second = tokio::time::timeout(
        Duration::from_secs(10),
        run_daemon(state.path(), budget(), std::future::pending::<()>()),
    )
    .await
    .expect("the second daemon kept running");

    assert_eq!(second.unwrap_err().code(), "daemon_running");
    stop.send(()).unwrap();
    first.await.unwrap().unwrap();
    assert!(!daemon_running(state.path()));
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_records_its_pid_while_it_runs() {
    let state = tempfile::tempdir().unwrap();
    assert_eq!(daemon_pid(state.path()), None);
    let (stop, daemon) = start(state.path());
    wait_for("the daemon to record its pid", || {
        daemon_pid(state.path()).is_some()
    })
    .await;

    assert_eq!(daemon_pid(state.path()), Some(std::process::id()));

    stop.send(()).unwrap();
    daemon.await.unwrap().unwrap();
    assert_eq!(daemon_pid(state.path()), None);
}

#[tokio::test]
async fn stopping_no_daemon_is_a_no_op() {
    let state = tempfile::tempdir().unwrap();
    assert_eq!(stop_daemon(state.path()).await.unwrap(), None);
    // A pid left by a daemon that exited is not signalled: no process holds
    // the lock.
    fs::write(state.path().join("daemon.lock"), "1\n").unwrap();
    assert_eq!(stop_daemon(state.path()).await.unwrap(), None);
}

/// The daemon checkpoints the registry as it exits, so removed workspaces
/// leave no grown WAL behind (#132).
#[tokio::test(flavor = "multi_thread")]
async fn daemon_truncates_the_registry_wal_on_exit() {
    let repo = Repo::new();
    let state = tempfile::tempdir().unwrap();
    let reg = Registry::open(state.path()).await.unwrap();
    let path = repo.worktree("one", "main");
    let now = SystemTime::now();
    let ws = Workspace {
        id: WorkspaceId::generate(),
        project: ProjectId::discover(&path).unwrap(),
        path,
        branch: None,
        created_at: now,
        last_active: now,
        adopted: false,
    };
    reg.insert(&ws).await.unwrap();
    reg.remove(&ws.id).await.unwrap();
    let wal = state.path().join("registry.sqlite3-wal");
    assert!(fs::metadata(&wal).unwrap().len() > 0);
    let (stop, daemon) = start(state.path());
    wait_for("the daemon to start", || daemon_running(state.path())).await;

    stop.send(()).unwrap();
    daemon.await.unwrap().unwrap();

    assert_eq!(fs::metadata(&wal).map_or(0, |meta| meta.len()), 0);
}

/// `openxet workspace status` probes clone support with a file named
/// `.openxet-clone-probe-*`; that is OpenXet's own write, not the
/// workspace's activity, and must not keep it from going idle.
#[tokio::test(flavor = "multi_thread")]
async fn clone_probes_are_not_activity() {
    let repo = Repo::new();
    let state = tempfile::tempdir().unwrap();
    let reg = Registry::open(state.path()).await.unwrap();
    let path = repo.worktree("one", "main");
    let idle_since = SystemTime::now() - Duration::from_hours(1);
    reg.insert(&Workspace {
        id: WorkspaceId::generate(),
        project: ProjectId::discover(&path).unwrap(),
        path: path.clone(),
        branch: None,
        created_at: idle_since,
        last_active: idle_since,
        adopted: false,
    })
    .await
    .unwrap();
    let (stop, daemon) = start(state.path());
    wait_for("the daemon to start", || daemon_running(state.path())).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let last_active = || async { reg.list(None).await.unwrap()[0].last_active };
    let before = last_active().await;

    let probe = path.join(".openxet-clone-probe-1-2");
    fs::write(&probe, [0u8]).unwrap();
    fs::remove_file(&probe).unwrap();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(last_active().await, before);

    // The watch was in place: a file of the workspace's own is activity.
    fs::write(path.join("notes.txt"), b"work").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while last_active().await == before {
        assert!(
            tokio::time::Instant::now() < deadline,
            "no activity recorded"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.send(()).unwrap();
    daemon.await.unwrap().unwrap();
}

/// Writes `bytes` to `path` with an mtime long past any settle window.
fn write_settled(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    let old = UNIX_EPOCH + Duration::new(1_761_600_000, 0);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_accessed(old).set_modified(old))
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn daemon_dedups_a_file_written_after_start() {
    let repo = Repo::new();
    let state = tempfile::tempdir().unwrap();
    let reg = Registry::open(state.path()).await.unwrap();
    let mut roots = Vec::new();
    for name in ["one", "two"] {
        let path = repo.worktree(name, "main");
        fs::create_dir_all(path.join("target/debug")).unwrap();
        let now = SystemTime::now();
        reg.insert(&Workspace {
            id: WorkspaceId::generate(),
            project: ProjectId::discover(&path).unwrap(),
            path: path.clone(),
            branch: Some(name.to_owned()),
            created_at: now,
            last_active: now - Duration::from_hours(1),
            adopted: false,
        })
        .await
        .unwrap();
        roots.push(path);
    }
    let probe = roots[0].join("target/probe");
    fs::write(&probe, b"probe").unwrap();
    if reflink_copy::reflink(&probe, roots[0].join("target/probe-clone")).is_err() {
        assert_ne!(std::env::consts::OS, "macos", "APFS can always clone");
        eprintln!("SKIPPED daemon_dedups_a_file_written_after_start: no clones");
        return;
    }
    let (stop, daemon) = start(state.path());
    wait_for("the daemon to start", || daemon_running(state.path())).await;
    // FSEvents and inotify start reporting once the watch is in place, a
    // moment after the lock is taken.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let bytes: Vec<u8> = (0..256u32 << 10).map(|i| (i % 251) as u8).collect();
    let first = roots[0].join("target/debug/out.rlib");
    let second = roots[1].join("target/debug/out.rlib");
    write_settled(&first, &bytes);
    write_settled(&second, &bytes);
    let inodes = (
        fs::metadata(&first).unwrap().ino(),
        fs::metadata(&second).unwrap().ino(),
    );

    // The swap renames a clone over one of them, which changes its inode.
    wait_for("one copy to be replaced by a clone", || {
        let now = (
            fs::metadata(&first).unwrap().ino(),
            fs::metadata(&second).unwrap().ino(),
        );
        now != inodes
    })
    .await;
    // Long enough for the swap's own events to come back: they must not
    // make the daemon clone the file again and count it twice.
    tokio::time::sleep(Duration::from_secs(1)).await;
    stop.send(()).unwrap();
    daemon.await.unwrap().unwrap();

    assert_eq!(fs::read(&first).unwrap(), bytes);
    assert_eq!(fs::read(&second).unwrap(), bytes);
    assert_eq!(reg.deduplicated_bytes().await.unwrap(), 256 << 10);
    let touched = reg.list(None).await.unwrap();
    assert!(
        touched.iter().any(
            |ws| SystemTime::now().duration_since(ws.last_active).unwrap() < Duration::from_mins(1)
        ),
        "no workspace's last_active was touched: {touched:?}"
    );
}
