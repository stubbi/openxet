#![allow(clippy::unwrap_used)]

mod support;

use std::fs;
use std::path::PathBuf;

use openxet_workspace::{UnsavedWork, add_worktree, unsaved_work};
use support::{Repo, git};

#[test]
fn create_makes_a_worktree_on_a_new_branch() {
    let repo = Repo::new();
    let base = git(&repo.main, &["rev-parse", "HEAD"]);
    let path = repo.root.join("worktrees/one");

    add_worktree(&repo.main, &path, "one", &base).unwrap();

    assert_eq!(git(&path, &["branch", "--show-current"]), "one");
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), base);
    assert!(path.join("lib.rs").is_file());
    let listed = git(&repo.main, &["worktree", "list", "--porcelain"]);
    assert!(listed.contains(&format!("worktree {}", path.display())));

    let err = add_worktree(&repo.main, &repo.root.join("two"), "one", &base).unwrap_err();
    assert_eq!(err.code(), "git", "an existing branch is refused: {err}");
}

#[test]
fn a_clean_workspace_has_no_unsaved_work() {
    let repo = Repo::new();
    let ws = repo.worktree("ws", "main");
    fs::create_dir_all(ws.join("target/debug")).unwrap();
    fs::write(ws.join("target/debug/out"), "build output").unwrap();

    assert_eq!(unsaved_work(&ws, Some("ws")).unwrap(), []);
}

#[test]
fn unsaved_work_names_each_file_that_exists_nowhere_else() {
    let repo = Repo::new();
    let ws = repo.worktree("ws", "main");
    fs::write(ws.join("lib.rs"), "fn main() { changed() }\n").unwrap();
    fs::write(ws.join("notes.md"), "draft").unwrap();
    // Ignored, but outside the presets: durable (spec §6.2a).
    fs::write(ws.join(".gitignore"), "/target/\n/results/\n").unwrap();
    fs::create_dir_all(ws.join("results")).unwrap();
    fs::write(ws.join("results/run.json"), "{}").unwrap();

    let mut found = unsaved_work(&ws, Some("ws")).unwrap();
    found.sort_by_key(ToString::to_string);
    assert_eq!(
        found,
        [
            UnsavedWork::Ignored(PathBuf::from("results/")),
            UnsavedWork::Uncommitted(PathBuf::from(".gitignore")),
            UnsavedWork::Uncommitted(PathBuf::from("lib.rs")),
            UnsavedWork::Untracked(PathBuf::from("notes.md")),
        ]
    );
}

#[test]
fn commits_count_as_saved_only_on_a_remote_another_branch_or_a_tag() {
    let repo = Repo::new();
    let ws = repo.worktree("ws", "main");
    Repo::commit(&ws, "a.rs", "a");
    let second = Repo::commit(&ws, "b.rs", "b");

    assert_eq!(
        unsaved_work(&ws, Some("ws")).unwrap(),
        [UnsavedWork::Unpushed {
            branch: "ws".to_owned(),
            commits: 2,
        }]
    );

    git(&repo.main, &["branch", "keep", &second]);
    assert_eq!(unsaved_work(&ws, Some("ws")).unwrap(), []);
    git(&repo.main, &["branch", "-D", "keep"]);

    git(&ws, &["push", "--quiet", "origin", "ws"]);
    assert_eq!(unsaved_work(&ws, Some("ws")).unwrap(), []);
}

#[test]
fn a_detached_head_reports_its_commits() {
    let repo = Repo::new();
    let ws = repo.worktree("ws", "main");
    git(&ws, &["checkout", "--quiet", "--detach"]);
    Repo::commit(&ws, "a.rs", "a");

    assert_eq!(
        unsaved_work(&ws, Some("ws")).unwrap(),
        [UnsavedWork::Unpushed {
            branch: "HEAD".to_owned(),
            commits: 1,
        }]
    );
}
