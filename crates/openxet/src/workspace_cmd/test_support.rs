//! A repository with a bare remote, and a [`Context`] with its own
//! registry, for the command tests.

use std::path::{Path, PathBuf};
use std::process::Command;

use openxet_workspace::{Volume, WorkspaceError};

use super::Context;

/// Runs `git -C dir args` with a fixed identity and none of the
/// developer's Git configuration, and returns its trimmed stdout.
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

/// An empty 1 TiB volume: neither a test machine's free space nor its size
/// may decide whether `create` passes the floor (a CI runner's temporary
/// volume can be smaller than the 20 GiB default floor).
#[allow(clippy::unnecessary_wraps)] // The signature `Context::volume` takes.
fn roomy(_: &Path) -> Result<Volume, WorkspaceError> {
    Ok(Volume {
        free_bytes: 1 << 40,
        total_bytes: 1 << 40,
    })
}

pub struct Fixture {
    _dir: tempfile::TempDir,
    pub main: PathBuf,
    pub context: Context,
}

impl Fixture {
    /// `main`, cloned from a bare remote with one pushed commit that
    /// ignores `/target/`, and a context running in it.
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let main = root.join("main");
        git(&root, &["init", "--quiet", "--bare", "remote.git"]);
        git(&root, &["init", "--quiet", "-b", "main", "main"]);
        let remote = root.join("remote.git");
        git(
            &main,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        std::fs::write(main.join(".gitignore"), "/target/\n").unwrap();
        std::fs::write(main.join("lib.rs"), "fn main() {}\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "--quiet", "-m", "first"]);
        git(&main, &["push", "--quiet", "-u", "origin", "main"]);
        let context = Context {
            state_dir: root.join("state"),
            cwd: main.clone(),
            config: None,
            volume: roomy,
            start_daemon: false,
        };
        Self {
            _dir: dir,
            main,
            context,
        }
    }

    /// Commits a new file in the worktree `dir`.
    pub fn commit(dir: &Path, file: &str) {
        std::fs::write(dir.join(file), file).unwrap();
        git(dir, &["add", file]);
        git(dir, &["commit", "--quiet", "-m", file]);
    }
}
