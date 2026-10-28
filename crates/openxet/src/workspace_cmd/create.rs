//! `openxet workspace create`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use openxet_workspace::{
    Presets, ProjectId, Registry, SETTLE_CAP, SeedReport, SeedSkip, SettleWait, Workspace,
    WorkspaceError, WorkspaceId, active_workspaces, add_worktree, branch_exists, delete_branch,
    free_to_floor, main_worktree, nearest_sibling, remove_worktree, resolve_commit, seed,
    seedable_bytes, wait_until_settled,
};
use serde::Serialize;

use super::{Context, absolute, daemon, human_bytes, names, workspace_error};
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
    /// How long seeding waited for the sibling's newest build outputs to
    /// settle, or for its Cargo build to finish.
    seed_waited_ms: u64,
    /// Whether the sibling was still building when the wait gave up, so
    /// only the outputs settled by then were cloned.
    seed_unsettled: bool,
    /// Whether this command started the workspace daemon, which was not
    /// running.
    daemon_started: bool,
    /// Why the daemon could not be started, if it couldn't.
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_error: Option<String>,
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
            (None, Some(from)) if self.seed_unsettled => format!(
                "{from} was still building after {} s, so only the {} of build outputs settled \
                 by then were cloned, sharing their disk blocks; the first build makes the rest.",
                self.seed_waited_ms / 1000,
                human_bytes(self.seeded_bytes)
            ),
            (None, Some(from)) if self.seed_waited_ms >= 1000 => format!(
                "Waited {:.1} s for {from}'s newest build outputs to settle, then cloned {} of \
                 them, sharing their disk blocks.",
                Duration::from_millis(self.seed_waited_ms).as_secs_f64(),
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
        if self.daemon_started {
            text.push_str(
                "\nStarted the workspace daemon: it deduplicates build outputs and evicts idle \
                 ones to keep free space over the floor.",
            );
        }
        if let Some(err) = &self.daemon_error {
            let _ = write!(
                text,
                "\nThe workspace daemon is not running ({err}), so nothing keeps the free-space \
                 floor; `openxet workspace status` shows whether it runs."
            );
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
        seed_waited_ms: 0,
        seed_unsettled: false,
        daemon_started: false,
        daemon_error: None,
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

    ensure_space(&registry, context, &path).await?;
    add_worktree(&dir, &path, &name, &base).map_err(error)?;
    let path = path.canonicalize().unwrap_or(path);
    // Seeded before it is registered, so the daemon starts watching it only
    // afterwards and never takes the seed's clones for new files to
    // deduplicate.
    let (report, from, wait) = seed_from_sibling(&registry, &project, &path, &base).await;
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
    let mut created = result(&path, report, from, false);
    if let Some(wait) = wait {
        created.seed_waited_ms = u64::try_from(wait.waited.as_millis()).unwrap_or(u64::MAX);
        created.seed_unsettled = !wait.settled;
    }
    if context.start_daemon {
        match daemon::start(&context.state_dir) {
            Ok(started) => created.daemon_started = started,
            Err(err) => created.daemon_error = Some(err),
        }
    }
    Ok(created)
}

/// Fails with `disk_budget` when free space on the new workspace's volume
/// is under the floor even after one eviction pass, so agents back off
/// instead of filling the disk.
async fn ensure_space(registry: &Registry, context: &Context, path: &Path) -> Result<(), CliError> {
    let error = |err: WorkspaceError| workspace_error(&err);
    let budget = context.budget()?;
    // The worktree doesn't exist yet; its nearest existing parent is on the
    // volume it will be on.
    let on = path.ancestors().find(|dir| dir.exists()).unwrap_or(path);
    let here = (context.volume)(on).map_err(error)?;
    if here.free_bytes >= budget.floor_for(here) {
        return Ok(());
    }
    free_to_floor(
        registry,
        &budget,
        SystemTime::now(),
        active_workspaces,
        context.volume,
    )
    .await
    .map_err(error)?;
    let here = (context.volume)(on).map_err(error)?;
    let floor_bytes = budget.floor_for(here);
    if here.free_bytes < floor_bytes {
        return Err(error(WorkspaceError::DiskBudget {
            free_bytes: here.free_bytes,
            floor_bytes,
        }));
    }
    Ok(())
}

/// Seeds the new workspace. Its worktree exists by now, so a failure here
/// is reported in the result rather than failing the command.
async fn seed_from_sibling(
    registry: &Registry,
    project: &ProjectId,
    path: &Path,
    base: &str,
) -> (SeedReport, Option<PathBuf>, Option<SettleWait>) {
    let failed = |err: WorkspaceError| SeedReport {
        cloned_bytes: 0,
        skipped: Some(SeedSkip::Failed(err.to_string())),
    };
    let presets = match Presets::load(path) {
        Ok(presets) => presets,
        Err(err) => return (failed(err), None, None),
    };
    match nearest_sibling(registry, project, path, base).await {
        // Seeding right after a sibling's build would skip its newest
        // outputs, which are still in the settle window, and the first
        // build would remake them.
        Ok(Some(from)) => {
            let wait = wait_until_settled(&from, &presets, SETTLE_CAP);
            (seed(path, Some(&from), &presets), Some(from), Some(wait))
        }
        Ok(None) => (seed(path, None, &presets), None, None),
        Err(err) => (failed(err), None, None),
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
    async fn create_waits_for_a_sibling_build_output_to_settle() {
        let fx = Fixture::new();
        let first = create(&args("first"), &fx.context).await.unwrap();
        let first_path = PathBuf::from(&first.path);
        fs::create_dir_all(first_path.join("target/debug")).unwrap();
        fs::write(first_path.join("target/debug/just-linked"), vec![5u8; 8192]).unwrap();

        let second = create(&args("second"), &fx.context).await.unwrap();

        assert!(second.seed_waited_ms >= 1500, "{}", second.seed_waited_ms);
        assert!(!second.seed_unsettled);
        if openxet_workspace::reflink_supported(&fx.main) {
            assert_eq!(second.seeded_bytes, 8192, "the settled file is cloned");
            assert!(second.human().contains("Waited"));
        }
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

    /// 1 GiB free of 100 GiB: under the default floor of 20 GiB.
    #[allow(clippy::unnecessary_wraps)] // The signature `Context::volume` takes.
    fn full(_: &Path) -> Result<openxet_workspace::Volume, WorkspaceError> {
        Ok(openxet_workspace::Volume {
            free_bytes: 1 << 30,
            total_bytes: 100 << 30,
        })
    }

    #[tokio::test]
    async fn create_fails_with_disk_budget_under_the_floor() {
        let mut fx = Fixture::new();
        let busy = create(&args("busy"), &fx.context).await.unwrap();
        fs::create_dir_all(Path::new(&busy.path).join("target")).unwrap();
        fs::write(Path::new(&busy.path).join("target/out"), [1u8; 4096]).unwrap();
        fx.context.volume = full;

        let err = create(&args("more"), &fx.context).await.unwrap_err();

        assert_eq!((err.code, err.exit), ("disk_budget", ExitCode::FAILURE));
        assert!(
            err.message.contains("1073741824 bytes free")
                && err.message.contains("floor of 21474836480 bytes"),
            "{}",
            err.message
        );
        assert!(err.hint.starts_with("Retry in a minute"), "{}", err.hint);
        assert!(!branch_exists(&fx.main, "more").unwrap());
        // Just created, so not idle: its build outputs stay.
        assert!(Path::new(&busy.path).join("target/out").exists());
    }

    #[tokio::test]
    async fn an_existing_name_is_refused() {
        let fx = Fixture::new();
        create(&args("taken"), &fx.context).await.unwrap();
        let err = create(&args("taken"), &fx.context).await.unwrap_err();
        assert_eq!((err.code, err.exit), ("exists", ExitCode::USAGE));
    }
}
