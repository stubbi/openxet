#![allow(clippy::unwrap_used)]

mod support;

use openxet_workspace::{Adoption, Registry, ScanSkip, adopt, adopt_scan};
use support::{Repo, git};

#[tokio::test]
async fn scan_registers_unknown_worktrees_once() {
    let repo = Repo::new();
    let state = repo.root.join("state");
    let registry = Registry::open(&state).await.unwrap();
    let one = repo.worktree("one", "main");
    let two = repo.worktree("two", "main");
    git(&two, &["checkout", "--quiet", "--detach"]);
    let gone = repo.worktree("gone", "main");
    std::fs::remove_dir_all(&gone).unwrap();

    let dry = adopt_scan(&registry, &repo.main, true).await.unwrap();
    assert_eq!(dry.adopted, [one.clone(), two.clone()]);
    assert!(registry.list(None).await.unwrap().is_empty());

    let first = adopt_scan(&registry, &repo.main, false).await.unwrap();

    assert_eq!(first.adopted, [one.clone(), two.clone()]);
    assert!(first.already.is_empty());
    assert_eq!(
        first.skipped,
        [
            (repo.main.clone(), ScanSkip::MainWorktree),
            (gone, ScanSkip::Missing)
        ]
    );
    let listed = registry.list(None).await.unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed.iter().all(|ws| ws.adopted));
    let by_path = |path| listed.iter().find(|ws| ws.path == path).unwrap();
    assert_eq!(by_path(one.clone()).branch.as_deref(), Some("one"));
    assert_eq!(by_path(two.clone()).branch, None);

    let second = adopt_scan(&registry, &one, false).await.unwrap();

    assert!(second.adopted.is_empty());
    assert_eq!(second.already, [one, two]);
    assert_eq!(registry.list(None).await.unwrap().len(), 2);
}

#[tokio::test]
async fn adopt_resolves_a_subdirectory_to_its_worktree_and_is_idempotent() {
    let repo = Repo::new();
    let registry = Registry::open(&repo.root.join("state")).await.unwrap();
    let ws = repo.worktree("ws", "main");
    std::fs::create_dir_all(ws.join("src/deep")).unwrap();

    let first = adopt(&registry, &ws.join("src/deep"), false).await.unwrap();
    let Adoption::Adopted(adopted) = first else {
        panic!("not adopted: {first:?}")
    };
    assert_eq!(
        (adopted.path.as_path(), adopted.adopted),
        (ws.as_path(), true)
    );

    let again = adopt(&registry, &ws, false).await.unwrap();
    assert!(matches!(again, Adoption::Already(ref w) if w.id == adopted.id));
}

#[tokio::test]
async fn the_main_worktree_is_never_adopted() {
    let repo = Repo::new();
    let registry = Registry::open(&repo.root.join("state")).await.unwrap();

    let err = adopt(&registry, &repo.main, false).await.unwrap_err();

    assert_eq!(err.code(), "main_worktree");
    assert!(registry.list(None).await.unwrap().is_empty());
}

#[tokio::test]
async fn adopt_outside_a_repository_is_not_a_repository() {
    let repo = Repo::new();
    let registry = Registry::open(&repo.root.join("state")).await.unwrap();
    let outside = repo.root.join("outside");
    std::fs::create_dir(&outside).unwrap();

    let err = adopt(&registry, &outside, false).await.unwrap_err();

    assert_eq!(err.code(), "not_a_repository");
}
