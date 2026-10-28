#![allow(clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use openxet_workspace::{
    HookCommands, WorktreeCreateInput, WorktreeRemoveInput, merge_claude_hooks, read_hook_input,
    write_atomically,
};
use serde_json::{Value, json};

const SETTINGS: &str = r#"{
  "permissions": {
    "allow": ["Bash(cargo test:*)"]
  },
  "env": {
    "ANTHROPIC_API_KEY": "sk-secret"
  },
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "lint-bash" }]
      }
    ]
  },
  "model": "opus"
}
"#;

fn commands() -> HookCommands {
    HookCommands::for_program("/opt/bin/openxet")
}

fn path() -> &'static Path {
    Path::new("/repo/.claude/settings.json")
}

#[test]
fn hook_input_is_read_with_unknown_fields_ignored() {
    let create: WorktreeCreateInput = read_hook_input(
        br#"{"session_id":"s","transcript_path":"/t","cwd":"/repo","hook_event_name":"WorktreeCreate","name":"bold-oak-a3f2","effort":"high"}"#,
    )
    .unwrap();
    assert_eq!(
        (create.cwd.as_path(), create.name.as_str()),
        (Path::new("/repo"), "bold-oak-a3f2")
    );
    let remove: WorktreeRemoveInput =
        read_hook_input(br#"{"cwd":"/repo","worktree_path":"/repo/.claude/worktrees/x"}"#).unwrap();
    assert_eq!(remove.worktree_path, Path::new("/repo/.claude/worktrees/x"));

    let err = read_hook_input::<WorktreeCreateInput>(b"{\"cwd\":\"/repo\"}").unwrap_err();
    assert_eq!(err.code(), "invalid_hook_input");
    assert!(err.to_string().contains("name"), "{err}");
}

#[test]
fn merge_keeps_every_key_in_order_and_other_tools_hooks() {
    let merged = merge_claude_hooks(path(), Some(SETTINGS), &commands()).unwrap();

    assert!(merged.changed);
    let value: Value = serde_json::from_str(&merged.text).unwrap();
    assert_eq!(value["permissions"]["allow"][0], "Bash(cargo test:*)");
    assert_eq!(value["env"]["ANTHROPIC_API_KEY"], "sk-secret");
    assert_eq!(value["model"], "opus");
    assert_eq!(
        value["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "lint-bash"
    );
    assert_eq!(
        value["hooks"]["WorktreeCreate"],
        json!([{"hooks": [{"type": "command", "command": "/opt/bin/openxet workspace hook claude-create"}]}])
    );
    assert_eq!(
        value["hooks"]["WorktreeRemove"][0]["hooks"][0]["command"],
        "/opt/bin/openxet workspace hook claude-remove"
    );
    // Untouched values keep their text, and keys their order.
    assert!(
        merged
            .text
            .starts_with("{\n  \"permissions\": {\n    \"allow\": [\"Bash(cargo test:*)\"]\n  },"),
        "{}",
        merged.text
    );
    let order: Vec<usize> = ["\"permissions\"", "\"env\"", "\"hooks\"", "\"model\""]
        .iter()
        .map(|key| merged.text.find(key).unwrap())
        .collect();
    assert!(order.is_sorted(), "{}", merged.text);
    assert!(merged.text.ends_with("}\n"));
}

#[test]
fn merging_twice_changes_nothing() {
    let once = merge_claude_hooks(path(), Some(SETTINGS), &commands()).unwrap();
    let twice = merge_claude_hooks(path(), Some(&once.text), &commands()).unwrap();

    assert!(!twice.changed);
    assert_eq!(twice.text, once.text);
}

#[test]
fn a_moved_binary_updates_openxets_hooks_in_place() {
    let old = HookCommands::for_program("/old/openxet");
    let once = merge_claude_hooks(path(), Some(SETTINGS), &old).unwrap();

    let moved = merge_claude_hooks(path(), Some(&once.text), &commands()).unwrap();

    assert!(moved.changed);
    let value: Value = serde_json::from_str(&moved.text).unwrap();
    let create = value["hooks"]["WorktreeCreate"].as_array().unwrap();
    assert_eq!(create.len(), 1);
    assert_eq!(
        create[0]["hooks"][0]["command"],
        "/opt/bin/openxet workspace hook claude-create"
    );
}

#[test]
fn another_worktree_hook_is_a_conflict() {
    let theirs =
        r#"{"hooks":{"WorktreeRemove":[{"hooks":[{"type":"command","command":"rm -rf x"}]}]}}"#;

    let err = merge_claude_hooks(path(), Some(theirs), &commands()).unwrap_err();

    assert_eq!(err.code(), "hook_conflict");
    assert!(err.to_string().contains("rm -rf x"), "{err}");
}

#[test]
fn no_file_or_an_empty_one_starts_from_an_empty_object() {
    for existing in [None, Some(""), Some(" \n")] {
        let merged = merge_claude_hooks(path(), existing, &commands()).unwrap();
        assert!(merged.changed);
        let value: Value = serde_json::from_str(&merged.text).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 1, "{}", merged.text);
    }
}

#[test]
fn invalid_settings_are_refused() {
    for bad in [
        "[]",
        "{",
        r#"{"hooks": []}"#,
        r#"{"hooks": {"WorktreeCreate": {}}}"#,
    ] {
        let err = merge_claude_hooks(path(), Some(bad), &commands()).unwrap_err();
        assert_eq!(err.code(), "invalid_settings", "{bad}");
    }
}

#[test]
fn the_display_form_redacts_env_values() {
    let merged = merge_claude_hooks(path(), Some(SETTINGS), &commands()).unwrap();

    assert!(!merged.display.contains("sk-secret"), "{}", merged.display);
    let value: Value = serde_json::from_str(&merged.display).unwrap();
    assert_eq!(value["env"]["ANTHROPIC_API_KEY"], "<redacted>");
    assert_eq!(
        value["hooks"],
        serde_json::from_str::<Value>(&merged.text).unwrap()["hooks"]
    );
}

#[test]
fn atomic_write_keeps_the_mode_and_writes_through_a_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("dotfiles/settings.json");
    std::fs::create_dir_all(real.parent().unwrap()).unwrap();
    std::fs::write(&real, "{}").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
    let link = dir.path().join("settings.json");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    write_atomically(&link, b"{\"a\": 1}\n", 0o600).unwrap();

    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "{\"a\": 1}\n");
    let mode = std::fs::metadata(&real).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640);

    let new = dir.path().join("new/.claude/settings.json");
    write_atomically(&new, b"{}\n", 0o600).unwrap();
    let mode = std::fs::metadata(&new).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let leftovers = std::fs::read_dir(new.parent().unwrap()).unwrap().count();
    assert_eq!(leftovers, 1, "no temporary file is left behind");
}
