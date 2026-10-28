#![allow(clippy::unwrap_used)]

mod support;

use std::fs::{self, File, FileTimes};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_workspace::{
    Budget, ProjectId, Registry, Workspace, WorkspaceId, daemon_running, run_daemon,
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
