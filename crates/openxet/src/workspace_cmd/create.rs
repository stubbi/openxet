//! `openxet workspace create`.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use openxet_workspace::{
    Presets, ProjectId, Registry, SeedReport, SeedSkip, Workspace, WorkspaceError, WorkspaceId,
    add_worktree, branch_exists, delete_branch, main_worktree, nearest_sibling, remove_worktree,
    resolve_commit, seed, seedable_bytes,
};
use serde::Serialize;

use super::{Context, absolute, human_bytes, names, workspace_error};
use crate::error::{CliError, ExitCode};
use crate::output::HumanDisplay;

#[derive(Debug, clap::Args)]
pub struct CreateArgs {
    /// A directory in the repository [default: the current directory]
    #[arg(long, value_name = "DIR")]
    pub(super) repo: Option<PathBuf>,
    /// The workspace's name, also its branch's: a slug of lowercase
    /// letters, digits and hyphens [default: a random `adjective-noun-xxxx`]
    #[arg(long, value_name = "SLUG")]
    pub(super) name: Option<String>,
    /// The commit to start from: a branch, tag or commit [default: `HEAD`]
    #[arg(long, value_name = "REV")]
    pub(super) base: Option<String>,
    /// Where to put the worktree; by default `<repo>/.claude/worktrees/<name>`
    /// if `<repo>/.claude` exists, else `<repo>/../<repo name>-worktrees/<name>`
    #[arg(long, value_name = "DIR")]
    pub(super) path: Option<PathBuf>,
    /// Report what would be created and seeded, and change nothing
    #[arg(long)]
    pub(super) dry_run: bool,
}

/// The `openxet.workspace.created.v1` result.
#[derive(Debug, Serialize)]
pub struct Created {
    schema: &'static str,
    pub(super) path: String,
    name: String,
    branch: String,
    /// The commit the branch starts at, in full.
    base: String,
    /// The size of the build outputs cloned, or with `dry_run` that would
    /// be; clones share their blocks, so this is not new disk use.
    seeded_bytes: u64,
    /// The worktree they were cloned from.
    seeded_from: Option<String>,
    /// `no_sibling`, `reflink_unsupported` or `failed`, if seeding stopped
    /// short.
    seed_skipped: Option<&'static str>,
    /// Why seeding failed, for `failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    seed_error: Option<String>,
    dry_run: bool,
}

impl HumanDisplay for Created {
    fn human(&self) -> String {
        let short = self.base.get(..12).unwrap_or(&self.base);
        let what = if self.dry_run {
            "Dry run: would create"
        } else {
            "Created"
        };
        let mut text = format!(
            "{what} workspace {} at {}, on new branch {} from {short}.",
            self.name, self.path, self.branch
        );
        let seeded = match (self.seed_skipped, &self.seeded_from) {
            (Some("no_sibling"), _) => {
                "No other worktree of this repository has build outputs to clone yet; the first \
                 build starts from scratch."
                    .to_owned()
            }
            (Some("reflink_unsupported"), _) => {
                "This filesystem can't clone files, so nothing was seeded: OpenXet never copies \
                 build outputs in full."
                    .to_owned()
            }
            (Some(_), _) => format!(
                "Seeding failed ({}), so the first build starts from scratch.",
                self.seed_error.as_deref().unwrap_or("no reason given")
            ),
            (None, Some(from)) if self.dry_run => format!(
                "Would clone {} of build outputs from {from}.",
                human_bytes(self.seeded_bytes)
            ),
            (None, Some(from)) => format!(
                "Cloned {} of build outputs from {from}, sharing their disk blocks.",
                human_bytes(self.seeded_bytes)
            ),
            (None, None) => String::new(),
        };
        if !seeded.is_empty() {
            text.push('\n');
            text.push_str(&seeded);
        }
        if self.dry_run {
            text.push_str("\nNothing was changed.");
        }
        text
    }
}

pub(super) async fn create(args: &CreateArgs, context: &Context) -> Result<Created, CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let dir = absolute(context, args.repo.as_deref().unwrap_or(Path::new(".")));
    let project = ProjectId::discover(&dir).map_err(error)?;
    let main = main_worktree(&dir).map_err(error)?;
    let name = match &args.name {
        Some(name) => names::check(name)?.to_owned(),
        None => names::random()?,
    };
    let path = match &args.path {
        Some(path) => absolute(context, path),
        None => default_path(&main, &name),
    };
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(exists(&format!("{} exists", path.display())));
    }
    if branch_exists(&dir, &name).map_err(error)? {
        return Err(exists(&format!("the branch {name} exists")));
    }
    let base = resolve_commit(&dir, args.base.as_deref().unwrap_or("HEAD")).map_err(error)?;
    let registry = Registry::open(&context.state_dir).await.map_err(error)?;
    let result = |path: &Path, report: SeedReport, from: Option<PathBuf>, dry_run| Created {
        schema: "openxet.workspace.created.v1",
        path: path.display().to_string(),
        name: name.clone(),
        branch: name.clone(),
        base: base.clone(),
        seeded_bytes: report.cloned_bytes,
        seeded_from: from.map(|from| from.display().to_string()),
        seed_skipped: report.skipped.as_ref().map(SeedSkip::code),
        seed_error: match report.skipped {
            Some(SeedSkip::Failed(reason)) => Some(reason),
            _ => None,
        },
        dry_run,
    };

    if args.dry_run {
        let from = nearest_sibling(&registry, &project, &path, &base)
            .await
            .map_err(error)?;
        let report = SeedReport {
            cloned_bytes: from
                .as_deref()
                .map_or(0, |from| seedable_bytes(from, &Presets::default())),
            skipped: from.is_none().then_some(SeedSkip::NoSibling),
        };
        return Ok(result(&path, report, from, true));
    }

    add_worktree(&dir, &path, &name, &base).map_err(error)?;
    let path = path.canonicalize().unwrap_or(path);
    let now = SystemTime::now();
    let workspace = Workspace {
        id: WorkspaceId::generate(),
        project: project.clone(),
        path: path.clone(),
        branch: Some(name.clone()),
        created_at: now,
        last_active: now,
        adopted: false,
    };
    if let Err(err) = registry.insert(&workspace).await {
        // Unregistered, nothing would ever evict or list it; it is empty, so
        // nothing is lost by taking it back.
        let _ = remove_worktree(&dir, &path);
        let _ = delete_branch(&dir, &name);
        return Err(error(err));
    }
    let (report, from) = seed_from_sibling(&registry, &project, &path, &base).await;
    Ok(result(&path, report, from, false))
}

/// Seeds the new workspace. It exists and is registered by now, so a
/// failure here is reported in the result rather than failing the command.
async fn seed_from_sibling(
    registry: &Registry,
    project: &ProjectId,
    path: &Path,
    base: &str,
) -> (SeedReport, Option<PathBuf>) {
    let failed = |err: WorkspaceError| SeedReport {
        cloned_bytes: 0,
        skipped: Some(SeedSkip::Failed(err.to_string())),
    };
    let presets = match Presets::load(path) {
        Ok(presets) => presets,
        Err(err) => return (failed(err), None),
    };
    match nearest_sibling(registry, project, path, base).await {
        Ok(from) => (seed(path, from.as_deref(), &presets), from),
        Err(err) => (failed(err), None),
    }
}

fn default_path(main: &Path, name: &str) -> PathBuf {
    if main.join(".claude").is_dir() {
        return main.join(".claude/worktrees").join(name);
    }
    let mut dir = main.file_name().unwrap_or_default().to_owned();
    dir.push("-worktrees");
    main.with_file_name(dir).join(name)
}

fn exists(what: &str) -> CliError {
    CliError::new(
        "exists",
        format!("{what}; nothing was created"),
        "Pass another `--name`, or `--path` for another directory.",
    )
    .with_exit(ExitCode::USAGE)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;
    use crate::workspace_cmd::test_support::{Fixture, git};

    fn args(name: &str) -> CreateArgs {
        CreateArgs {
            repo: None,
            name: Some(name.to_owned()),
            base: None,
            path: None,
            dry_run: false,
        }
    }

    #[tokio::test]
    async fn create_outside_a_repository_is_not_a_repository() {
        let fx = Fixture::new();
        let outside = fx.main.parent().unwrap().join("outside");
        fs::create_dir(&outside).unwrap();
        let mut args = args("one");
        args.repo = Some(outside);

        let err = create(&args, &fx.context).await.unwrap_err();

        assert_eq!(err.code, "not_a_repository");
        assert_eq!(err.exit, ExitCode::FAILURE);
        assert!(!err.hint.is_empty());
    }

    #[tokio::test]
    async fn create_registers_a_worktree_and_seeds_it_from_a_sibling() {
        let fx = Fixture::new();
        let first = create(&args("first"), &fx.context).await.unwrap();
        assert_eq!(first.seed_skipped, Some("no_sibling"));
        let first_path = PathBuf::from(&first.path);
        assert_eq!(
            first_path,
            fx.main.parent().unwrap().join("main-worktrees/first")
        );
        assert_eq!(git(&first_path, &["branch", "--show-current"]), "first");
        assert_eq!(first.base, git(&fx.main, &["rev-parse", "HEAD"]));
        fs::create_dir_all(first_path.join("target/debug")).unwrap();
        fs::write(first_path.join("target/debug/out"), vec![3u8; 8192]).unwrap();
        let old = SystemTime::now() - Duration::from_hours(1);
        fs::File::open(first_path.join("target/debug/out"))
            .unwrap()
            .set_modified(old)
            .unwrap();

        let second = create(&args("second"), &fx.context).await.unwrap();

        assert_eq!(second.seeded_from.as_deref(), Some(first.path.as_str()));
        if openxet_workspace::reflink_supported(&fx.main) {
            assert_eq!((second.seed_skipped, second.seeded_bytes), (None, 8192));
        } else {
            assert_eq!(second.seed_skipped, Some("reflink_unsupported"));
        }
        let registry = Registry::open(&fx.context.state_dir).await.unwrap();
        assert_eq!(registry.list(None).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn create_dry_run_changes_nothing() {
        let fx = Fixture::new();
        let mut args = args("dry");
        args.dry_run = true;

        let created = create(&args, &fx.context).await.unwrap();

        assert!(created.dry_run);
        assert!(!Path::new(&created.path).exists());
        let registry = Registry::open(&fx.context.state_dir).await.unwrap();
        assert!(registry.list(None).await.unwrap().is_empty());
        assert!(!branch_exists(&fx.main, "dry").unwrap());
    }

    #[tokio::test]
    async fn an_existing_name_is_refused() {
        let fx = Fixture::new();
        create(&args("taken"), &fx.context).await.unwrap();
        let err = create(&args("taken"), &fx.context).await.unwrap_err();
        assert_eq!((err.code, err.exit), ("exists", ExitCode::USAGE));
    }
}
