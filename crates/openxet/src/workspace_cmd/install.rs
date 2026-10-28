//! `openxet workspace install claude`: puts OpenXet's worktree hooks into a
//! Claude Code settings file.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use openxet_workspace::{
    HookCommands, WorkspaceError, merge_claude_hooks, worktree_root, write_atomically,
};
use serde::Serialize;

use super::{Context, absolute, workspace_error};
use crate::error::CliError;
use crate::output::HumanDisplay;

#[derive(Debug, clap::Subcommand)]
pub enum InstallCommand {
    /// Install the `WorktreeCreate` and `WorktreeRemove` hooks into Claude
    /// Code's settings, so its worktrees are OpenXet workspaces
    Claude(ClaudeArgs),
}

#[derive(Debug, clap::Args)]
pub struct ClaudeArgs {
    /// Into the repository's `.claude/settings.json`, shared with everyone
    /// who clones it
    #[arg(long, conflicts_with = "user")]
    project: bool,
    /// Into your `~/.claude/settings.json`, for every repository (the
    /// default)
    #[arg(long)]
    user: bool,
    /// With `--project`, a directory in the repository [default: the
    /// current directory]
    #[arg(long, value_name = "DIR", requires = "project")]
    repo: Option<PathBuf>,
    /// Print the merged settings, and write nothing
    #[arg(long)]
    dry_run: bool,
}

/// Which `openxet` the hooks run.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Program {
    /// This binary, at a path that stays put.
    Absolute(PathBuf),
    /// `openxet` as the shell finds it on `PATH`.
    OnPath(Why),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Why {
    /// A project's settings are shared, so they can't name this machine's
    /// binary.
    Shared,
    /// This binary is at a path that may move: a build output, a temporary
    /// directory or a versioned package directory.
    Moves,
}

/// The `openxet.workspace.installed.v1` result.
#[derive(Debug, Serialize)]
pub struct Installed {
    schema: &'static str,
    settings_path: String,
    /// `user` or `project`.
    scope: &'static str,
    /// `absolute`: the hooks run `binary`; `path`: they run `openxet` from
    /// `PATH`.
    program: &'static str,
    binary: Option<String>,
    create_command: String,
    remove_command: String,
    /// Whether the file is changed, or with `dry_run` would be.
    changed: bool,
    dry_run: bool,
    /// The settings as merged, every value under `env` shown as
    /// `<redacted>`.
    settings: serde_json::Value,
    #[serde(skip)]
    display: String,
    #[serde(skip)]
    why: Option<Why>,
}

impl HumanDisplay for Installed {
    fn human(&self) -> String {
        let runs = match (&self.binary, self.why) {
            (Some(binary), _) => format!("The hooks run {binary}."),
            (None, Some(Why::Shared)) => "The hooks run `openxet` from PATH: a project's \
                 settings are shared, so they don't name this machine's binary."
                .to_owned(),
            (None, _) => "The hooks run `openxet` from PATH: this binary is at a path that may \
                 move (a build output or a temporary directory), so put `openxet` on the PATH \
                 Claude Code runs with."
                .to_owned(),
        };
        if self.dry_run {
            let what = if self.changed {
                "would write"
            } else {
                "would leave unchanged"
            };
            return format!(
                "Dry run: {what} {}:\n{}{runs}\nNothing was changed.",
                self.settings_path, self.display
            );
        }
        if self.changed {
            format!(
                "Installed Claude Code's WorktreeCreate and WorktreeRemove hooks in {}. {runs}\n\
                 Worktrees Claude Code makes from now on are OpenXet workspaces; `openxet \
                 workspace adopt --scan` registers the ones it made before.",
                self.settings_path
            )
        } else {
            format!(
                "{} already has OpenXet's hooks; nothing changed. {runs}",
                self.settings_path
            )
        }
    }
}

pub(super) fn claude(args: &ClaudeArgs, context: &Context) -> Result<Installed, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let (scope, settings_path, new_mode) = if args.project {
        let dir = absolute(context, args.repo.as_deref().unwrap_or(Path::new(".")));
        let root = worktree_root(&dir).map_err(error)?;
        ("project", root.join(".claude/settings.json"), 0o644)
    } else {
        let home = context.home.as_deref().ok_or_else(|| {
            CliError::new(
                "io",
                "no home directory to find ~/.claude/settings.json in",
                "Set HOME, or pass `--project` to install into a repository's settings.",
            )
        })?;
        // Its `env` often holds API keys.
        ("user", home.join(".claude/settings.json"), 0o600)
    };
    let program = if args.project {
        Program::OnPath(Why::Shared)
    } else {
        program(
            std::env::current_exe().ok().as_deref(),
            std::env::var_os("PATH").as_deref(),
        )
    };
    let (program, word) = match program {
        Program::Absolute(path) => match shell_word(&path) {
            Some(word) => (Program::Absolute(path), word),
            None => (Program::OnPath(Why::Moves), "openxet".to_owned()),
        },
        on_path @ Program::OnPath(_) => (on_path, "openxet".to_owned()),
    };
    let commands = HookCommands::for_program(&word);
    let existing = match std::fs::read_to_string(&settings_path) {
        Ok(text) => Some(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(error(WorkspaceError::Io {
                path: settings_path,
                source,
            }));
        }
    };
    let merged =
        merge_claude_hooks(&settings_path, existing.as_deref(), &commands).map_err(error)?;
    if merged.changed && !args.dry_run {
        write_atomically(&settings_path, merged.text.as_bytes(), new_mode).map_err(error)?;
    }
    let (binary, why) = match &program {
        Program::Absolute(path) => (Some(path.display().to_string()), None),
        Program::OnPath(why) => (None, Some(*why)),
    };
    Ok(Installed {
        schema: "openxet.workspace.installed.v1",
        settings_path: settings_path.display().to_string(),
        scope,
        program: if binary.is_some() { "absolute" } else { "path" },
        binary,
        create_command: commands.create,
        remove_command: commands.remove,
        changed: merged.changed,
        dry_run: args.dry_run,
        settings: serde_json::from_str(&merged.display).unwrap_or_default(),
        display: merged.display,
        why,
    })
}

/// Which `openxet` user-wide hooks should run: the `PATH` entry that is
/// this binary, so a package manager's stable link is named rather than
/// the versioned file behind it; else this binary, unless its path may
/// move; else `openxet` from `PATH`.
fn program(exe: Option<&Path>, path_var: Option<&OsStr>) -> Program {
    let Some(exe) = exe else {
        return Program::OnPath(Why::Moves);
    };
    let real = exe.canonicalize().ok();
    let on_path = path_var
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("openxet"))
        .find(|candidate| real.is_some() && candidate.canonicalize().ok() == real);
    match on_path {
        Some(found) if !moves(&found) => Program::Absolute(found),
        _ if exe.is_absolute() && !moves(exe) => Program::Absolute(exe.to_path_buf()),
        _ => Program::OnPath(Why::Moves),
    }
}

/// Whether `path` may not hold this binary tomorrow: a Cargo build output
/// (`target/debug`, `target/release`, with or without a target triple), a
/// temporary directory, or a Homebrew or Nix versioned directory.
fn moves(path: &Path) -> bool {
    let names: Vec<&OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    let build_output = names.iter().enumerate().any(|(at, name)| {
        *name == "target"
            && names[at + 1..]
                .iter()
                .any(|later| *later == "debug" || *later == "release")
    });
    let temporary = [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/var/folders"),
        PathBuf::from("/private/var/folders"),
    ]
    .iter()
    .any(|dir| path.starts_with(dir));
    build_output
        || temporary
        || names.iter().any(|name| *name == "Cellar")
        || path.starts_with("/nix/store")
}

/// `path` as one POSIX shell word, single-quoted if it needs to be; `None`
/// if it is not UTF-8.
fn shell_word(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let plain = text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"/._-+@%:,".contains(&b));
    Some(if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_outputs_and_temporary_paths_move() {
        for moving in [
            "/src/openxet/target/debug/openxet",
            "/src/openxet/target/aarch64-apple-darwin/release/openxet",
            "/tmp/x/openxet",
            "/opt/homebrew/Cellar/openxet/0.1.0/bin/openxet",
            "/nix/store/abc-openxet/bin/openxet",
        ] {
            assert!(moves(Path::new(moving)), "{moving}");
        }
        for staying in [
            "/Users/a/.cargo/bin/openxet",
            "/usr/local/bin/openxet",
            "/opt/homebrew/bin/openxet",
            "/home/a/target-practice/bin/openxet",
        ] {
            assert!(!moves(Path::new(staying)), "{staying}");
        }
    }

    #[test]
    fn a_path_entry_that_is_this_binary_is_named() {
        let dir = tempfile::tempdir().unwrap();
        // Outside the temporary directory, which moves by definition.
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(
            program(Some(&bin.join("Cargo.toml")), None),
            Program::Absolute(bin.join("Cargo.toml"))
        );
        assert_eq!(
            program(Some(Path::new("/src/target/debug/openxet")), None),
            Program::OnPath(Why::Moves)
        );
        assert_eq!(program(None, None), Program::OnPath(Why::Moves));
        let linked = dir.path().join("openxet");
        std::os::unix::fs::symlink(bin.join("Cargo.toml"), &linked).unwrap();
        // Found on PATH, but in a temporary directory: this binary instead.
        assert_eq!(
            program(Some(&bin.join("Cargo.toml")), Some(dir.path().as_os_str())),
            Program::Absolute(bin.join("Cargo.toml"))
        );
    }

    #[test]
    fn paths_are_quoted_for_the_shell() {
        assert_eq!(
            shell_word(Path::new("/usr/local/bin/openxet")).unwrap(),
            "/usr/local/bin/openxet"
        );
        assert_eq!(
            shell_word(Path::new("/Users/a b/it's/openxet")).unwrap(),
            r"'/Users/a b/it'\''s/openxet'"
        );
    }
}
