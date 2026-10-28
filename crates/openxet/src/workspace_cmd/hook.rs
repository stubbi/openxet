//! `openxet workspace hook`: Claude Code's `WorktreeCreate` and
//! `WorktreeRemove` hooks. The contract they keep is documented, with the
//! date it was verified, in `openxet_workspace`'s `hooks` module.

use std::io::{IsTerminal as _, Read as _};
use std::time::SystemTime;

use openxet_workspace::{
    Adoption, Registry, WorkspaceError, WorktreeCreateInput, WorktreeRemoveInput, adopt,
    main_worktree, read_hook_input,
};
use serde::Serialize;

use super::create::{CreateArgs, Created, create, default_path};
use super::remove::{Removed, already_gone, remove_workspace};
use super::{Context, absolute, daemon, names, workspace_error};
use crate::error::CliError;
use crate::output::HumanDisplay;

/// Claude Code sends a few hundred bytes; anything near this is not its
/// hook input.
const MAX_INPUT: u64 = 1 << 20;

#[derive(Debug, clap::Subcommand)]
pub enum HookCommand {
    /// Claude Code's `WorktreeCreate` hook: reads its JSON on stdin, creates
    /// the workspace and prints only its path
    ClaudeCreate(HookArgs),
    /// Claude Code's `WorktreeRemove` hook: reads its JSON on stdin and
    /// removes the workspace, refusing if that would lose work
    ClaudeRemove(HookArgs),
}

#[derive(Debug, clap::Args)]
pub struct HookArgs {
    /// Report what would be created or removed, and change nothing
    #[arg(long)]
    dry_run: bool,
}

/// The `openxet.workspace.hook_created.v1` result. Without `--json` only
/// `path` is printed, as the one line Claude Code reads.
#[derive(Debug, Serialize)]
pub struct HookCreated {
    schema: &'static str,
    path: String,
    /// Whether the workspace existed already, as when Claude Code resumes
    /// a session in it.
    reused: bool,
    /// What `create` did; `None` when reused.
    created: Option<Created>,
    dry_run: bool,
}

impl HumanDisplay for HookCreated {
    fn human(&self) -> String {
        self.path.clone()
    }
}

impl HookCreated {
    /// What happened, for people reading Claude Code's hook log on stderr.
    pub(super) fn summary(&self) -> String {
        self.created.as_ref().map_or_else(
            || format!("Reusing workspace {}.", self.path),
            HumanDisplay::human,
        )
    }
}

/// Reads the hook's JSON from stdin.
pub(super) fn read_stdin() -> Result<Vec<u8>, CliError> {
    if std::io::stdin().is_terminal() {
        // Waiting for typed JSON would look like a hang.
        return Err(workspace_error(&WorkspaceError::HookInput(
            "stdin is a terminal, not the hook's JSON".to_owned(),
        )));
    }
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT)
        .read_to_end(&mut input)
        .map_err(|err| {
            CliError::new(
                "io",
                format!("could not read the hook's input from stdin: {err}"),
                "Pipe Claude Code's hook JSON into the command.",
            )
        })?;
    Ok(input)
}

pub(super) async fn claude_create(
    input: &[u8],
    args: &HookArgs,
    context: &Context,
) -> Result<HookCreated, CliError> {
    let input: WorktreeCreateInput = read_hook_input(input).map_err(|err| workspace_error(&err))?;
    let name = names::slug(&input.name)?;
    let create_args = CreateArgs {
        repo: Some(input.cwd.clone()),
        name: Some(name.clone()),
        base: None,
        path: None,
        dry_run: args.dry_run,
    };
    match create(&create_args, context).await {
        Ok(created) => Ok(HookCreated {
            schema: "openxet.workspace.hook_created.v1",
            path: created.path.clone(),
            reused: false,
            created: Some(created),
            dry_run: args.dry_run,
        }),
        Err(err) if err.code == "exists" => reuse(&input, &name, args, context).await?.ok_or(err),
        Err(err) => Err(err),
    }
}

/// The registered workspace at the path `create` would have used for
/// `name`, if there is one: Claude Code asks again for a worktree it made
/// before when a session resumes in it.
async fn reuse(
    input: &WorktreeCreateInput,
    name: &str,
    args: &HookArgs,
    context: &Context,
) -> Result<Option<HookCreated>, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let cwd = absolute(context, &input.cwd);
    let path = default_path(&main_worktree(&cwd).map_err(error)?, name);
    let path = path.canonicalize().unwrap_or(path);
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let Some(workspace) = registry.get_by_path(&path).await.map_err(error)? else {
        return Ok(None);
    };
    if !args.dry_run {
        registry
            .touch(&workspace.id, SystemTime::now())
            .await
            .map_err(error)?;
        if context.start_daemon {
            // Reported by `create` when it starts it; here the workspace is
            // usable either way, and `status` tells whether it runs.
            let _ = daemon::start(&context.state_dir);
        }
    }
    Ok(Some(HookCreated {
        schema: "openxet.workspace.hook_created.v1",
        path: workspace.path.display().to_string(),
        reused: true,
        created: None,
        dry_run: args.dry_run,
    }))
}

pub(super) async fn claude_remove(
    input: &[u8],
    args: &HookArgs,
    context: &Context,
) -> Result<Removed, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let input: WorktreeRemoveInput = read_hook_input(input).map_err(|err| workspace_error(&err))?;
    let path = absolute(context, &input.worktree_path);
    let path = path.canonicalize().unwrap_or(path);
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let workspace = match registry.get_by_path(&path).await.map_err(error)? {
        Some(workspace) => workspace,
        // Exit 0 tells Claude Code it is removed, which it is.
        None if !path.exists() => return Ok(already_gone(&path, args.dry_run)),
        // Made before the hooks were installed, or by Claude Code's own
        // Git fallback: adopted first, so its branch is kept.
        None => match adopt(&registry, &path, args.dry_run).await {
            Ok(Adoption::Adopted(workspace) | Adoption::Already(workspace)) => workspace,
            Err(WorkspaceError::NotARepository { .. }) => {
                return Err(error(WorkspaceError::NotAWorkspace(
                    path.display().to_string(),
                )));
            }
            Err(err) => return Err(error(err)),
        },
    };
    remove_workspace(&registry, &workspace, false, args.dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_cmd::test_support::Fixture;

    fn input(fx: &Fixture, name: &str) -> Vec<u8> {
        serde_json::json!({"cwd": fx.main, "name": name, "hook_event_name": "WorktreeCreate"})
            .to_string()
            .into_bytes()
    }

    #[tokio::test]
    async fn claude_create_dry_run_creates_nothing() {
        let fx = Fixture::new();

        let created = claude_create(&input(&fx, "dry"), &HookArgs { dry_run: true }, &fx.context)
            .await
            .unwrap();

        assert!(!created.reused);
        assert!(!std::path::Path::new(&created.path).exists());
        let registry = Registry::open(&fx.context.state_dir).await.unwrap();
        assert!(registry.list(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn claude_remove_of_a_gone_unregistered_path_succeeds() {
        let fx = Fixture::new();
        let gone = fx.main.parent().unwrap().join("never-made");
        let input = serde_json::json!({ "worktree_path": gone }).to_string();

        let removed = claude_remove(input.as_bytes(), &HookArgs { dry_run: false }, &fx.context)
            .await
            .unwrap();

        assert_eq!(
            removed.human(),
            format!("Removed {}, freeing up to 0 B.", gone.display())
        );
    }
}
