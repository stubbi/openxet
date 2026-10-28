#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_workspace::{ProjectId, Registry, Workspace, WorkspaceId};

fn git_repo(dir: &Path) -> ProjectId {
    let status = Command::new("git")
        .args(["init", "--quiet"])
        .arg(dir)
        .status()
        .unwrap();
    assert!(status.success());
    ProjectId::discover(dir).unwrap()
}

fn workspace(project: &ProjectId, path: &Path) -> Workspace {
    Workspace {
        id: WorkspaceId::generate(),
        project: project.clone(),
        path: path.to_path_buf(),
        branch: Some("agent/one".to_owned()),
        created_at: UNIX_EPOCH + Duration::from_nanos(1_761_600_000_123_456_789),
        last_active: UNIX_EPOCH + Duration::from_nanos(1_761_600_100_987_654_321),
        adopted: false,
    }
}

#[tokio::test]
async fn registry_round_trips_a_workspace() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let other_repo = tempfile::tempdir().unwrap();
    let project = git_repo(repo.path());
    let other = git_repo(other_repo.path());
    assert_ne!(project, other);
    let registry = Registry::open(state.path()).await.unwrap();

    let ws = workspace(&project, &repo.path().join("wt/one"));
    let adopted = Workspace {
        id: WorkspaceId::generate(),
        path: other_repo.path().join("wt/two"),
        project: other.clone(),
        branch: None,
        adopted: true,
        ..ws.clone()
    };
    registry.insert(&ws).await.unwrap();
    registry.insert(&adopted).await.unwrap();

    assert_eq!(
        registry.get_by_path(&ws.path).await.unwrap(),
        Some(ws.clone())
    );
    assert_eq!(
        registry.get_by_path(&adopted.path).await.unwrap(),
        Some(adopted.clone())
    );
    assert_eq!(
        registry.list(Some(&project)).await.unwrap(),
        vec![ws.clone()]
    );
    assert_eq!(registry.list(None).await.unwrap().len(), 2);

    registry.remove(&ws.id).await.unwrap();
    assert_eq!(registry.get_by_path(&ws.path).await.unwrap(), None);
    let err = registry.remove(&ws.id).await.unwrap_err();
    assert_eq!(err.code(), "not_a_workspace");
}

#[tokio::test]
async fn touch_updates_last_active() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let project = git_repo(repo.path());
    let registry = Registry::open(state.path()).await.unwrap();
    let ws = workspace(&project, &repo.path().join("wt"));
    registry.insert(&ws).await.unwrap();

    let later = ws.last_active + Duration::from_hours(1);
    registry.touch(&ws.id, later).await.unwrap();

    let stored = registry.get_by_path(&ws.path).await.unwrap().unwrap();
    assert_eq!(stored.last_active, later);
    assert_eq!(stored.created_at, ws.created_at);

    let err = registry
        .touch(&WorkspaceId::generate(), SystemTime::now())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_a_workspace");
}

#[tokio::test]
async fn canonical_index_returns_the_last_set_path() {
    let state = tempfile::tempdir().unwrap();
    let registry = Registry::open(state.path()).await.unwrap();
    let hash = [7u8; 32];

    assert_eq!(registry.canonical(1 << 20, hash).await.unwrap(), None);
    registry
        .set_canonical(1 << 20, hash, Path::new("/ws/a/target/libx.rlib"))
        .await
        .unwrap();
    registry
        .set_canonical(1 << 20, hash, Path::new("/ws/b/target/libx.rlib"))
        .await
        .unwrap();

    assert_eq!(
        registry.canonical(1 << 20, hash).await.unwrap(),
        Some(PathBuf::from("/ws/b/target/libx.rlib"))
    );
    assert_eq!(registry.canonical(1 << 21, hash).await.unwrap(), None);
    assert_eq!(registry.canonical(1 << 20, [8u8; 32]).await.unwrap(), None);
}

#[tokio::test]
async fn forget_canonical_drops_entries_under_a_path() {
    let state = tempfile::tempdir().unwrap();
    let registry = Registry::open(state.path()).await.unwrap();
    let entries = [
        ([1u8; 32], "/ws/one"),
        ([2u8; 32], "/ws/one/target/debug/deps/liba.rlib"),
        ([3u8; 32], "/ws/one-more/target/libb.rlib"),
        ([4u8; 32], "/ws/two/target/libc.rlib"),
    ];
    for (hash, path) in entries {
        registry
            .set_canonical(4096, hash, Path::new(path))
            .await
            .unwrap();
    }

    registry
        .forget_canonical(Path::new("/ws/one"))
        .await
        .unwrap();

    assert_eq!(registry.canonical(4096, [1u8; 32]).await.unwrap(), None);
    assert_eq!(registry.canonical(4096, [2u8; 32]).await.unwrap(), None);
    assert_eq!(
        registry.canonical(4096, [3u8; 32]).await.unwrap(),
        Some(PathBuf::from("/ws/one-more/target/libb.rlib"))
    );
    assert_eq!(
        registry.canonical(4096, [4u8; 32]).await.unwrap(),
        Some(PathBuf::from("/ws/two/target/libc.rlib"))
    );
}

#[tokio::test]
async fn registry_survives_reopen() {
    let state = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let project = git_repo(repo.path());
    let ws = workspace(&project, &repo.path().join("wt"));
    {
        let registry = Registry::open(state.path()).await.unwrap();
        registry.insert(&ws).await.unwrap();
        registry
            .set_canonical(65536, [9u8; 32], &ws.path.join("target/x"))
            .await
            .unwrap();
    }

    let registry = Registry::open(state.path()).await.unwrap();
    assert_eq!(
        registry.get_by_path(&ws.path).await.unwrap(),
        Some(ws.clone())
    );
    assert_eq!(
        registry.canonical(65536, [9u8; 32]).await.unwrap(),
        Some(ws.path.join("target/x"))
    );
}
