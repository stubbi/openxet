//! Claude Code's `WorktreeCreate` and `WorktreeRemove` hooks: their stdin,
//! and merging OpenXet's into a Claude Code settings file.
//!
//! The contract, verified on 2026-10-28 against
//! <https://code.claude.com/docs/en/hooks> (sections `WorktreeCreate`,
//! `WorktreeRemove` and "Exit code output") and
//! <https://code.claude.com/docs/en/settings>:
//!
//! - **Input.** Both get a JSON object on stdin with the common fields
//!   (`session_id`, `transcript_path`, `cwd`, `hook_event_name`, and others
//!   that are not listed for every event). `WorktreeCreate` adds `name`, a
//!   slug "either specified by the user or auto-generated" such as
//!   `bold-oak-a3f2`; `WorktreeRemove` adds `worktree_path`, the absolute
//!   path of the worktree being removed. Unknown fields are ignored here, so
//!   new ones don't break the hook.
//! - **Create's output.** A command hook prints the worktree's path as the
//!   last non-empty line of stdout; everything else belongs on stderr. It
//!   can't return JSON, since stdout is read as the path. Claude Code
//!   refuses an absolute path with `.` or `..` segments or one through a
//!   symlink below the repository root, so the path printed is canonical.
//!   Configuring the hook replaces Claude Code's own `git worktree`
//!   creation (and `.worktreeinclude` processing) entirely.
//! - **Exit codes.** For `WorktreeCreate`, any non-zero exit fails the
//!   creation. For `WorktreeRemove`, exit 0 counts the worktree as removed
//!   (the hook must have deleted it); a non-zero exit fails the removal if
//!   the directory still exists, and it stays with no Git fallback, which is
//!   how a workspace with unsaved work is kept.
//! - **When remove runs.** Not when a subagent or workflow agent finishes:
//!   Claude Code keeps those worktrees and skips the hook, which is why
//!   `adopt --scan` and the idle TTL exist.
//! - **Settings.** Neither event takes a matcher (one is ignored). The
//!   shape is `{"hooks": {"WorktreeCreate": [{"hooks": [{"type":
//!   "command", "command": "…"}]}]}}`, in `~/.claude/settings.json` (user),
//!   `.claude/settings.json` (shared project) or
//!   `.claude/settings.local.json`; entries from every level are merged. The
//!   default command timeout, 600 s, is left as it is.
//!
//! The docs don't say how several `WorktreeCreate` hooks combine, so
//! [`merge_claude_hooks`] refuses to install beside another tool's.

use std::fs::{self, File};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::de::{DeserializeOwned, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use serde_json::value::RawValue;

use crate::WorkspaceError;

/// The arguments of `openxet` that run the `WorktreeCreate` hook.
pub const CLAUDE_CREATE_ARGS: &str = "workspace hook claude-create";

/// The arguments of `openxet` that run the `WorktreeRemove` hook.
pub const CLAUDE_REMOVE_ARGS: &str = "workspace hook claude-remove";

const CREATE_EVENT: &str = "WorktreeCreate";
const REMOVE_EVENT: &str = "WorktreeRemove";

/// What a settings value under `env` is shown as: it often holds an API
/// key, and credentials are never echoed.
const REDACTED: &str = "<redacted>";

/// `WorktreeCreate`'s stdin.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeCreateInput {
    /// The directory Claude Code runs in, inside the repository.
    pub cwd: PathBuf,
    /// The worktree's name.
    pub name: String,
}

/// `WorktreeRemove`'s stdin.
#[derive(Debug, Clone, Deserialize)]
pub struct WorktreeRemoveInput {
    /// The absolute path of the worktree to remove.
    pub worktree_path: PathBuf,
}

/// Parses a hook's stdin.
///
/// # Errors
///
/// [`WorkspaceError::HookInput`] if it is not JSON with the fields `T`
/// needs.
pub fn read_hook_input<T: DeserializeOwned>(stdin: &[u8]) -> Result<T, WorkspaceError> {
    serde_json::from_slice(stdin).map_err(|err| WorkspaceError::HookInput(err.to_string()))
}

/// The shell commands Claude Code runs for the two hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommands {
    /// For `WorktreeCreate`.
    pub create: String,
    /// For `WorktreeRemove`.
    pub remove: String,
}

impl HookCommands {
    /// The commands that run `program`, a shell word naming `openxet`
    /// (quoted already if it needs to be).
    #[must_use]
    pub fn for_program(program: &str) -> Self {
        Self {
            create: format!("{program} {CLAUDE_CREATE_ARGS}"),
            remove: format!("{program} {CLAUDE_REMOVE_ARGS}"),
        }
    }
}

/// A settings file with OpenXet's hooks merged in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedSettings {
    /// The file's new text; the old text, byte for byte, when unchanged.
    pub text: String,
    /// `text` with every value under `env` replaced by `<redacted>`, to
    /// show.
    pub display: String,
    /// Whether `text` differs from the file.
    pub changed: bool,
}

/// Merges OpenXet's `WorktreeCreate` and `WorktreeRemove` hooks into the
/// settings `existing` (`None` for no file). Every other key and hook is
/// kept, keys in their order and untouched values in their own text. An
/// OpenXet hook already there (any command running
/// [`CLAUDE_CREATE_ARGS`] or [`CLAUDE_REMOVE_ARGS`]) is pointed at
/// `commands`, so a second merge changes nothing.
///
/// # Errors
///
/// [`WorkspaceError::Settings`] if `existing` is not a JSON object with
/// `hooks` an object of arrays, and [`WorkspaceError::HookConflict`] if
/// another tool has a hook for either event.
pub fn merge_claude_hooks(
    path: &Path,
    existing: Option<&str>,
    commands: &HookCommands,
) -> Result<MergedSettings, WorkspaceError> {
    let invalid = |message: String| WorkspaceError::Settings {
        path: path.to_path_buf(),
        message,
    };
    let existing = existing.filter(|text| !text.trim().is_empty());
    let mut top = match existing {
        Some(text) => {
            parse_object(text).map_err(|err| invalid(format!("not a JSON object: {err}")))?
        }
        None => Vec::new(),
    };
    let hooks_at = top.iter().position(|(key, _)| key == "hooks");
    let mut hooks = match hooks_at {
        Some(at) => {
            parse_object(&top[at].1).map_err(|_| invalid("`hooks` is not an object".to_owned()))?
        }
        None => Vec::new(),
    };
    let mut changed = false;
    for (event, command, args) in [
        (CREATE_EVENT, &commands.create, CLAUDE_CREATE_ARGS),
        (REMOVE_EVENT, &commands.remove, CLAUDE_REMOVE_ARGS),
    ] {
        let at = hooks.iter().position(|(key, _)| key == event);
        let mut groups: Vec<Value> = match at {
            Some(at) => serde_json::from_str(&hooks[at].1)
                .map_err(|_| invalid(format!("`hooks.{event}` is not an array")))?,
            None => Vec::new(),
        };
        if !point_at(&mut groups, command, args).map_err(|handler| {
            WorkspaceError::HookConflict {
                path: path.to_path_buf(),
                event,
                handler,
            }
        })? {
            continue;
        }
        changed = true;
        let rendered = pretty_at(&Value::Array(groups), 2);
        match at {
            Some(at) => hooks[at].1 = rendered,
            None => hooks.push((event.to_owned(), rendered)),
        }
    }
    let text = if changed {
        let rendered = render_object(&hooks, 1);
        match hooks_at {
            Some(at) => top[at].1 = rendered,
            None => top.push(("hooks".to_owned(), rendered)),
        }
        render_object(&top, 0) + "\n"
    } else {
        existing.unwrap_or_default().to_owned()
    };
    let display = match top.iter().position(|(key, _)| key == "env") {
        Some(at) => {
            let mut shown = top.clone();
            shown[at].1 = redact(&shown[at].1);
            render_object(&shown, 0) + "\n"
        }
        None => text.clone(),
    };
    Ok(MergedSettings {
        text,
        display,
        changed,
    })
}

/// Points every OpenXet handler in `groups` at `command`, adding one group
/// if there is none. Returns whether anything changed, or the first other
/// handler, which is a conflict.
fn point_at(groups: &mut Vec<Value>, command: &str, args: &str) -> Result<bool, String> {
    let mut found = false;
    let mut changed = false;
    for handler in groups
        .iter_mut()
        .filter_map(|group| group.get_mut("hooks").and_then(Value::as_array_mut))
        .flatten()
    {
        let theirs = handler.get("command").and_then(Value::as_str);
        let ours = handler.get("type").and_then(Value::as_str) == Some("command")
            && theirs.is_some_and(|theirs| theirs.contains(args));
        if !ours {
            return Err(theirs.map_or_else(
                || {
                    let kind = handler.get("type").and_then(Value::as_str);
                    format!("a {} hook", kind.unwrap_or("typeless"))
                },
                |theirs| format!("`{theirs}`"),
            ));
        }
        found = true;
        if theirs != Some(command) {
            handler["command"] = Value::String(command.to_owned());
            changed = true;
        }
    }
    if !found {
        groups.push(serde_json::json!({
            "hooks": [{"type": "command", "command": command}]
        }));
        changed = true;
    }
    Ok(changed)
}

/// A JSON object's members in their order, each value as its own text.
struct Members(Vec<(String, String)>);

impl<'de> Deserialize<'de> for Members {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;

        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = Members;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Members, A::Error> {
                let mut members = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, Box<RawValue>>()? {
                    members.push((key, value.get().to_owned()));
                }
                Ok(Members(members))
            }
        }

        deserializer.deserialize_map(ObjectVisitor)
    }
}

fn parse_object(text: &str) -> Result<Vec<(String, String)>, serde_json::Error> {
    serde_json::from_str::<Members>(text).map(|members| members.0)
}

/// `members` as an object pretty-printed at nesting `depth`, two spaces a
/// level as Claude Code writes its settings.
fn render_object(members: &[(String, String)], depth: usize) -> String {
    if members.is_empty() {
        return "{}".to_owned();
    }
    let indent = "  ".repeat(depth + 1);
    let lines: Vec<String> = members
        .iter()
        .map(|(key, value)| format!("{indent}{}: {value}", Value::String(key.clone())))
        .collect();
    format!("{{\n{}\n{}}}", lines.join(",\n"), "  ".repeat(depth))
}

/// `value` pretty-printed to sit at nesting `depth`.
fn pretty_at(value: &Value, depth: usize) -> String {
    let indent = format!("\n{}", "  ".repeat(depth));
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| value.to_string())
        .replace('\n', &indent)
}

/// The `env` value `text`, each of its values replaced by [`REDACTED`].
fn redact(text: &str) -> String {
    let shown = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(vars)) => Value::Object(
            vars.into_iter()
                .map(|(name, _)| (name, Value::String(REDACTED.to_owned())))
                .collect(),
        ),
        _ => Value::String(REDACTED.to_owned()),
    };
    pretty_at(&shown, 1)
}

/// Replaces the file at `path` with `contents` all at once: written to a
/// temporary file beside it, flushed, then renamed over it, so a crash
/// leaves the old file or the new one, never half of one. A symlink is
/// written through, so dotfile managers keep their link. The file keeps its
/// permissions; a new one gets `new_mode`. Missing parent directories are
/// created.
///
/// # Errors
///
/// [`WorkspaceError::Io`] if any step fails; the old file is then intact.
pub fn write_atomically(path: &Path, contents: &[u8], new_mode: u32) -> Result<(), WorkspaceError> {
    let target = if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        path.canonicalize().map_err(WorkspaceError::io(path))?
    } else {
        path.to_path_buf()
    };
    let dir = target
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir).map_err(WorkspaceError::io(dir))?;
    let mode = fs::metadata(&target).map_or(new_mode, |m| m.permissions().mode() & 0o7777);
    let mut temp = tempfile::Builder::new()
        .prefix(".openxet-")
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(WorkspaceError::io(dir))?;
    let io = WorkspaceError::io(temp.path().to_path_buf());
    temp.write_all(contents)
        .and_then(|()| {
            temp.as_file()
                .set_permissions(fs::Permissions::from_mode(mode))
        })
        .and_then(|()| temp.as_file().sync_all())
        .map_err(io)?;
    temp.persist(&target)
        .map_err(|err| WorkspaceError::io(&target)(err.error))?;
    // The rename is durable once the directory is.
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(WorkspaceError::io(dir))
}
