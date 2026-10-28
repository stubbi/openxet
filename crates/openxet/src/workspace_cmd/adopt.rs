//! `openxet workspace adopt`: registers worktrees made without OpenXet.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use openxet_workspace::{Adoption, Registry, ScanReport, WorkspaceError, adopt, adopt_scan};
use serde::Serialize;

use super::{Context, absolute, workspace_error};
use crate::error::CliError;
use crate::output::HumanDisplay;

#[derive(Debug, clap::Args)]
pub struct AdoptArgs {
    /// A directory in the worktree to adopt
    #[arg(
        value_name = "PATH",
        required_unless_present = "scan",
        conflicts_with = "scan"
    )]
    path: Option<PathBuf>,
    /// Adopt every worktree of the repository that is not registered yet
    #[arg(long)]
    scan: bool,
    /// With `--scan`, a directory in the repository [default: the current
    /// directory]
    #[arg(long, value_name = "DIR", requires = "scan")]
    repo: Option<PathBuf>,
    /// Report what would be adopted, and change nothing
    #[arg(long)]
    dry_run: bool,
}

/// The `openxet.workspace.adopted.v1` result.
#[derive(Debug, Serialize)]
pub struct Adoptions {
    schema: &'static str,
    /// Worktrees newly registered, or with `dry_run` that would be.
    adopted: Vec<String>,
    /// Worktrees registered already.
    already: Vec<String>,
    /// Worktrees `--scan` left out: the main worktree, which is never
    /// adopted, and worktrees whose directory is gone.
    skipped: Vec<Skipped>,
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct Skipped {
    path: String,
    /// `main_worktree` or `missing`.
    reason: &'static str,
}

impl HumanDisplay for Adoptions {
    fn human(&self) -> String {
        let verb = if self.dry_run {
            "Would adopt"
        } else {
            "Adopted"
        };
        let mut text = String::new();
        for path in &self.adopted {
            let _ = writeln!(text, "{verb} {path}.");
        }
        for path in &self.already {
            let _ = writeln!(text, "Already registered: {path}.");
        }
        for skipped in &self.skipped {
            let why = match skipped.reason {
                "main_worktree" => "the main worktree is never adopted",
                _ => "its directory is gone; `git worktree prune` forgets it",
            };
            let _ = writeln!(text, "Skipped {}: {why}.", skipped.path);
        }
        if self.adopted.is_empty() && self.already.is_empty() && self.skipped.is_empty() {
            text.push_str("No worktrees to adopt.\n");
        }
        if !self.adopted.is_empty() && !self.dry_run {
            text.push_str(
                "Adopted workspaces' build outputs are evicted when idle; their branches are \
                 kept on `remove`.\n",
            );
        }
        if self.dry_run {
            text.push_str("Nothing was changed.\n");
        }
        text.pop();
        text
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

pub(super) async fn run(args: &AdoptArgs, context: &Context) -> Result<Adoptions, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let report = if args.scan {
        let repo = absolute(context, args.repo.as_deref().unwrap_or(Path::new(".")));
        adopt_scan(&registry, &repo, args.dry_run)
            .await
            .map_err(error)?
    } else {
        let path = absolute(context, args.path.as_deref().unwrap_or(Path::new(".")));
        let mut report = ScanReport::default();
        match adopt(&registry, &path, args.dry_run).await.map_err(error)? {
            Adoption::Adopted(ws) => report.adopted.push(ws.path),
            Adoption::Already(ws) => report.already.push(ws.path),
        }
        report
    };
    Ok(Adoptions {
        schema: "openxet.workspace.adopted.v1",
        adopted: report.adopted.iter().map(|p| display(p)).collect(),
        already: report.already.iter().map(|p| display(p)).collect(),
        skipped: report
            .skipped
            .iter()
            .map(|(path, why)| Skipped {
                path: display(path),
                reason: why.code(),
            })
            .collect(),
        dry_run: args.dry_run,
    })
}
