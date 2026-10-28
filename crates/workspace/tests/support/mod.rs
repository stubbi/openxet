//! Git repositories for the integration tests, isolated from the
//! developer's Git configuration.

#![allow(clippy::unwrap_used, dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs `git -C dir args` with a fixed identity and no global or system
/// config (a signing or hook setting there would break the tests), and
/// returns its trimmed stdout.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A repository cloned from a bare remote, with one pushed commit that
/// ignores `/target/`. Paths are canonical (macOS's temp dir is behind a
/// symlink).
pub struct Repo {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
    pub main: PathBuf,
}

impl Repo {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let remote = root.join("remote.git");
        let main = root.join("main");
        git(&root, &["init", "--quiet", "--bare", "remote.git"]);
        git(&root, &["init", "--quiet", "-b", "main", "main"]);
        git(
            &main,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(main.join(".gitignore"), "/target/\n").unwrap();
        std::fs::write(main.join("lib.rs"), "fn main() {}\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "--quiet", "-m", "first"]);
        git(&main, &["push", "--quiet", "-u", "origin", "main"]);
        Self { dir, root, main }
    }

    /// Commits a change to `file` in the worktree `dir` and returns the new
    /// commit.
    pub fn commit(dir: &Path, file: &str, text: &str) -> String {
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "--quiet", "-m", file]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A new worktree at `root/name` on a new branch `name` from `base`.
    pub fn worktree(&self, name: &str, base: &str) -> PathBuf {
        let path = self.root.join(name);
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                name,
                path.to_str().unwrap(),
                base,
            ],
        );
        path
    }
}
