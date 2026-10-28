//! `openxet workspace hook` and `install`, run as the real binary the way
//! Claude Code runs them: JSON on stdin, the result on stdout, in a temp
//! state directory and a temp `HOME`, so no real settings or registry is
//! touched.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    main: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
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
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

impl Setup {
    /// A repository with a `.claude` directory and a pushed commit, and a
    /// `HOME` whose workspace config sets a 1 GiB floor, so a full test
    /// machine still creates workspaces.
    fn new() -> Self {
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
        std::fs::write(main.join(".gitignore"), "/target/\n/.claude/worktrees/\n").unwrap();
        std::fs::write(main.join("lib.rs"), "fn main() {}\n").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["commit", "--quiet", "-m", "first"]);
        git(&main, &["push", "--quiet", "-u", "origin", "main"]);
        std::fs::create_dir(main.join(".claude")).unwrap();
        let home = root.join("home");
        let config = home.join(".config/openxet/workspace.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(config, "floor = \"1 GiB\"\n").unwrap();
        let state = root.join("state");
        Self {
            _dir: dir,
            root,
            main,
            home,
            state,
        }
    }

    fn openxet(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = common::openxet();
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("XDG_CONFIG_HOME")
            .env("OPENXET_WORKSPACE_STATE", &self.state)
            .env("OPENXET_WORKSPACE_DAEMON", "off")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        cmd
    }

    /// Runs `openxet args` in `cwd` with `stdin` piped in.
    fn run(&self, cwd: &Path, args: &[&str], stdin: &[u8]) -> Output {
        let mut child = self
            .openxet(cwd, args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin).unwrap();
        child.wait_with_output().unwrap()
    }

    /// Runs the `WorktreeCreate` hook for `name`, as Claude Code would.
    fn claude_create(&self, name: &str) -> Output {
        let input = json!({
            "session_id": "abc123",
            "transcript_path": "/nonexistent/transcript.jsonl",
            "cwd": self.main,
            "hook_event_name": "WorktreeCreate",
            "name": name,
        });
        self.run(
            &self.root,
            &["workspace", "hook", "claude-create"],
            input.to_string().as_bytes(),
        )
    }

    fn registered(&self) -> Value {
        let out = self.run(&self.root, &["workspace", "list", "--json"], b"");
        common::json_stdout(&out)
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn claude_create_prints_only_the_path_on_stdout() {
    let setup = Setup::new();

    let out = setup.claude_create("bold-oak-a3f2");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = stdout(&out);
    let expected = setup.main.join(".claude/worktrees/bold-oak-a3f2");
    assert_eq!(printed, format!("{}\n", expected.display()));
    assert!(expected.join("lib.rs").is_file());
    assert_eq!(
        git(&expected, &["branch", "--show-current"]),
        "bold-oak-a3f2"
    );
    let listed = setup.registered();
    assert_eq!(
        listed["workspaces"][0]["path"],
        expected.display().to_string()
    );

    // Claude Code resuming the same worktree gets the same path back.
    let again = setup.claude_create("bold-oak-a3f2");
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(stdout(&again), printed);
    assert_eq!(
        setup.registered()["workspaces"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn claude_create_turns_a_name_that_is_no_slug_into_one() {
    let setup = Setup::new();

    let out = setup.claude_create("Feature/Auth");

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = setup.main.join(".claude/worktrees/feature-auth");
    assert_eq!(stdout(&out), format!("{}\n", expected.display()));
}

#[test]
fn claude_create_fails_without_a_path_on_error() {
    let setup = Setup::new();
    let outside = setup.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    let input = json!({"cwd": outside, "name": "x", "hook_event_name": "WorktreeCreate"});

    let out = setup.run(
        &setup.root,
        &["workspace", "hook", "claude-create"],
        input.to_string().as_bytes(),
    );

    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "stdout: {}", stdout(&out));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not inside a Git repository"));

    let garbage = setup.run(
        &setup.root,
        &["workspace", "hook", "claude-create", "--json"],
        b"not json",
    );
    assert!(garbage.stdout.is_empty());
    let err = common::json_error(&garbage, 2);
    assert_eq!(err["code"], "invalid_hook_input");
}

#[test]
fn claude_remove_keeps_a_workspace_with_unsaved_work() {
    let setup = Setup::new();
    let created = setup.claude_create("busy");
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let path = PathBuf::from(stdout(&created).trim_end());
    assert!(path.is_absolute());
    std::fs::write(path.join("notes.md"), "draft").unwrap();
    let input =
        json!({"cwd": setup.main, "hook_event_name": "WorktreeRemove", "worktree_path": path});
    let remove = |setup: &Setup| {
        setup.run(
            &setup.root,
            &["workspace", "hook", "claude-remove", "--json"],
            input.to_string().as_bytes(),
        )
    };

    let out = remove(&setup);

    let err = common::json_error(&out, 1);
    assert_eq!(err["code"], "unsaved_work");
    assert!(path.join("notes.md").is_file());
    assert_eq!(
        setup.registered()["workspaces"].as_array().unwrap().len(),
        1
    );

    std::fs::remove_file(path.join("notes.md")).unwrap();
    let out = remove(&setup);
    let removed = common::json_stdout(&out);
    assert_eq!(removed["schema"], "openxet.workspace.removed.v1");
    assert_eq!(removed["deleted_branch"], "busy");
    assert!(!path.exists());
    assert!(
        setup.registered()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn claude_remove_never_removes_the_main_worktree() {
    let setup = Setup::new();
    let input = json!({"worktree_path": setup.main});

    let out = setup.run(
        &setup.root,
        &["workspace", "hook", "claude-remove", "--json"],
        input.to_string().as_bytes(),
    );

    assert_eq!(common::json_error(&out, 2)["code"], "main_worktree");
    assert!(setup.main.join("lib.rs").is_file());
}

const EXISTING: &str = r#"{
  "permissions": {
    "allow": ["Bash(cargo test:*)"]
  },
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit",
        "hooks": [{ "type": "command", "command": "cargo fmt" }]
      }
    ]
  }
}
"#;

#[test]
fn install_merges_hooks_into_existing_settings() {
    let setup = Setup::new();
    let settings = setup.main.join(".claude/settings.json");
    std::fs::write(&settings, EXISTING).unwrap();
    let install = ["workspace", "install", "claude", "--project", "--json"];

    let out = setup.run(&setup.main, &install, b"");

    let result = common::json_stdout(&out);
    assert_eq!(result["schema"], "openxet.workspace.installed.v1");
    assert_eq!(result["changed"], true);
    assert_eq!(result["program"], "path");
    assert_eq!(result["settings_path"], settings.display().to_string());
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(written["permissions"]["allow"][0], "Bash(cargo test:*)");
    assert_eq!(
        written["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
        "cargo fmt"
    );
    assert_eq!(
        written["hooks"]["WorktreeCreate"][0]["hooks"][0],
        json!({"type": "command", "command": "openxet workspace hook claude-create"})
    );
    assert_eq!(
        written["hooks"]["WorktreeRemove"][0]["hooks"][0]["command"],
        "openxet workspace hook claude-remove"
    );

    let before = std::fs::read(&settings).unwrap();
    let again = common::json_stdout(&setup.run(&setup.main, &install, b""));
    assert_eq!(again["changed"], false);
    assert_eq!(std::fs::read(&settings).unwrap(), before);
}

#[test]
fn install_dry_run_prints_the_merged_settings() {
    let setup = Setup::new();
    let settings = setup.main.join(".claude/settings.json");
    std::fs::write(&settings, EXISTING).unwrap();

    let out = setup.run(
        &setup.main,
        &["workspace", "install", "claude", "--project", "--dry-run"],
        b"",
    );

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let printed = stdout(&out);
    assert!(printed.contains("\"WorktreeCreate\""), "{printed}");
    assert!(
        printed.contains("openxet workspace hook claude-remove"),
        "{printed}"
    );
    assert!(printed.contains("\"cargo fmt\""), "{printed}");
    assert!(printed.contains("Nothing was changed."), "{printed}");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), EXISTING);

    let json = common::json_stdout(&setup.run(
        &setup.main,
        &[
            "workspace",
            "install",
            "claude",
            "--project",
            "--dry-run",
            "--json",
        ],
        b"",
    ));
    assert_eq!(json["dry_run"], true);
    assert_eq!(
        json["settings"]["hooks"]["WorktreeCreate"][0]["hooks"][0]["command"],
        "openxet workspace hook claude-create"
    );
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), EXISTING);
}

#[test]
fn install_user_writes_settings_under_home() {
    let setup = Setup::new();

    let out = setup.run(
        &setup.root,
        &["workspace", "install", "claude", "--user", "--json"],
        b"",
    );

    let result = common::json_stdout(&out);
    let settings = setup.home.join(".claude/settings.json");
    assert_eq!(result["settings_path"], settings.display().to_string());
    // The test binary is a Cargo build output, which moves; so PATH.
    assert_eq!(result["program"], "path");
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(
        written["hooks"]["WorktreeCreate"][0]["hooks"][0]["command"],
        "openxet workspace hook claude-create"
    );
}

#[test]
fn adopt_registers_a_worktree_made_with_git() {
    let setup = Setup::new();
    let made = setup.root.join("by-hand");
    git(
        &setup.main,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "by-hand",
            made.to_str().unwrap(),
        ],
    );

    let out = setup.run(&made, &["workspace", "adopt", ".", "--json"], b"");

    let adopted = common::json_stdout(&out);
    assert_eq!(adopted["schema"], "openxet.workspace.adopted.v1");
    assert_eq!(adopted["adopted"], json!([made.display().to_string()]));
    let again = common::json_stdout(&setup.run(&made, &["workspace", "adopt", ".", "--json"], b""));
    assert_eq!(again["already"], json!([made.display().to_string()]));

    let main = setup.run(&setup.main, &["workspace", "adopt", ".", "--json"], b"");
    assert_eq!(common::json_error(&main, 2)["code"], "main_worktree");
}
