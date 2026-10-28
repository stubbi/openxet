use std::ffi::OsStr;
use std::fmt;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crate::{Presets, WorkspaceError};

/// Runs `git -C <dir> <args>` and returns its output whatever its exit
/// status, so callers can tell Git's answers (`check-ignore` exits 1 for
/// "not ignored") from failures.
///
/// The variables that point Git at another repository are cleared: when
/// OpenXet runs inside a Git hook they are set for the hook's repository,
/// not for `dir`.
pub(crate) fn run(dir: &Path, args: &[&str]) -> Result<Output, WorkspaceError> {
    run_os(dir, &args.iter().map(OsStr::new).collect::<Vec<_>>())
}

/// [`run`] for arguments that are paths, which need not be UTF-8.
fn run_os(dir: &Path, args: &[&OsStr]) -> Result<Output, WorkspaceError> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|err| WorkspaceError::Git {
            command: command_line(args),
            message: format!("could not run git ({err}); install Git and put it on PATH"),
        })
}

fn command_line(args: &[&OsStr]) -> String {
    args.iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The `Git` error for a command that exited unsuccessfully.
pub(crate) fn failed(args: &[&str], output: &Output) -> WorkspaceError {
    failed_os(&args.iter().map(OsStr::new).collect::<Vec<_>>(), output)
}

fn failed_os(args: &[&OsStr], output: &Output) -> WorkspaceError {
    WorkspaceError::Git {
        command: command_line(args),
        message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    }
}

/// Runs a Git command that must succeed and returns its stdout.
pub(crate) fn stdout(dir: &Path, args: &[&str]) -> Result<Vec<u8>, WorkspaceError> {
    let output = run(dir, args)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failed(args, &output))
    }
}

fn stdout_line(dir: &Path, args: &[&str]) -> Result<String, WorkspaceError> {
    let out = stdout(dir, args)?;
    Ok(String::from_utf8_lossy(&out).trim_end().to_owned())
}

/// Adds a worktree of `repo` at `path`, on a new branch `branch` starting
/// at `base` (`git worktree add -b`).
///
/// # Errors
///
/// [`WorkspaceError::Git`] if Git refuses, for example because `branch`
/// exists or `path` is a non-empty directory.
pub fn add_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> Result<(), WorkspaceError> {
    let args = [
        OsStr::new("worktree"),
        OsStr::new("add"),
        OsStr::new("--quiet"),
        OsStr::new("-b"),
        OsStr::new(branch),
        path.as_os_str(),
        OsStr::new(base),
    ];
    let output = run_os(repo, &args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(failed_os(&args, &output))
    }
}

/// Removes the worktree at `path` with everything in it, or, if its
/// directory is already gone, prunes Git's record of it.
///
/// # Errors
///
/// [`WorkspaceError::Git`] if Git refuses, for example for a locked
/// worktree.
pub fn remove_worktree(repo: &Path, path: &Path) -> Result<(), WorkspaceError> {
    if !path.exists() {
        stdout(repo, &["worktree", "prune"])?;
        return Ok(());
    }
    // `--force` because a workspace's build outputs are untracked files,
    // which Git otherwise refuses to delete; `unsaved_work` has already
    // been asked, or the caller passed `--discard`.
    let args = [
        OsStr::new("worktree"),
        OsStr::new("remove"),
        OsStr::new("--force"),
        path.as_os_str(),
    ];
    let output = run_os(repo, &args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(failed_os(&args, &output))
    }
}

/// Deletes the local branch `branch` whatever it holds (`git branch -D`),
/// unless another worktree has it checked out. Returns whether it was
/// deleted.
///
/// # Errors
///
/// [`WorkspaceError::Git`] if Git fails.
pub fn delete_branch(repo: &Path, branch: &str) -> Result<bool, WorkspaceError> {
    let reference = format!("refs/heads/{branch}");
    if !run(repo, &["show-ref", "--verify", "--quiet", &reference])?
        .status
        .success()
    {
        return Ok(false);
    }
    let listed = stdout(repo, &["worktree", "list", "--porcelain", "-z"])?;
    let checked_out = listed
        .split(|&b| b == 0)
        .any(|line| line.strip_prefix(b"branch ") == Some(reference.as_bytes()));
    if checked_out {
        return Ok(false);
    }
    stdout(repo, &["branch", "--quiet", "-D", branch])?;
    Ok(true)
}

/// Whether the local branch `branch` exists.
///
/// # Errors
///
/// [`WorkspaceError::Git`] if Git can't run.
pub fn branch_exists(repo: &Path, branch: &str) -> Result<bool, WorkspaceError> {
    let reference = format!("refs/heads/{branch}");
    Ok(run(repo, &["show-ref", "--verify", "--quiet", &reference])?
        .status
        .success())
}

/// The commit `rev` names in `repo`, as a full hex id.
///
/// # Errors
///
/// [`WorkspaceError::UnknownRevision`] if it names no commit.
pub fn resolve_commit(repo: &Path, rev: &str) -> Result<String, WorkspaceError> {
    let spec = format!("{rev}^{{commit}}");
    let output = run(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            &spec,
        ],
    )?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(WorkspaceError::UnknownRevision(rev.to_owned()))
    }
}

/// The repository's main worktree, the first `git worktree list` names,
/// whichever of its worktrees `dir` is in. For a bare repository, its Git
/// directory.
///
/// # Errors
///
/// [`WorkspaceError::Git`] if Git fails.
pub fn main_worktree(dir: &Path) -> Result<PathBuf, WorkspaceError> {
    let listed = stdout(dir, &["worktree", "list", "--porcelain", "-z"])?;
    listed
        .split(|&b| b == 0)
        .find_map(|line| line.strip_prefix(b"worktree "))
        .map(|path| PathBuf::from(OsStr::from_bytes(path)))
        .ok_or_else(|| WorkspaceError::Git {
            command: "worktree list --porcelain -z".to_owned(),
            message: "Git listed no worktree".to_owned(),
        })
}

/// The commit checked out in the worktree `dir`, or `None` before its first
/// commit.
pub(crate) fn head(dir: &Path) -> Option<String> {
    stdout_line(dir, &["rev-parse", "--verify", "--quiet", "HEAD"]).ok()
}

/// How many commits lie between `a` and `b`, on either side
/// (`git rev-list --count a...b`).
pub(crate) fn distance(repo: &Path, a: &str, b: &str) -> Option<u64> {
    stdout_line(repo, &["rev-list", "--count", &format!("{a}...{b}")])
        .ok()?
        .parse()
        .ok()
}

/// Work in a workspace that exists nowhere else, so removing the workspace
/// would lose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsavedWork {
    /// A tracked file with changes, staged or not, relative to the root.
    Uncommitted(PathBuf),
    /// A file or directory Git doesn't track or ignore.
    Untracked(PathBuf),
    /// A file or directory Git ignores outside the reconstructible presets,
    /// which spec §6.2a counts as durable: results, local config, secrets.
    Ignored(PathBuf),
    /// Commits on `branch` (or `HEAD` when detached) that no remote-tracking
    /// branch, tag or other local branch reaches.
    Unpushed {
        /// The branch, or `HEAD`.
        branch: String,
        /// How many commits.
        commits: u32,
    },
}

impl UnsavedWork {
    /// The stable name of this kind, as `openxet.error.v1` reports it.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Uncommitted(_) => "uncommitted",
            Self::Untracked(_) => "untracked",
            Self::Ignored(_) => "ignored",
            Self::Unpushed { .. } => "unpushed",
        }
    }
}

impl fmt::Display for UnsavedWork {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Uncommitted(path) => write!(f, "uncommitted changes to {}", path.display()),
            Self::Untracked(path) => write!(f, "untracked {}", path.display()),
            Self::Ignored(path) => write!(f, "ignored {}", path.display()),
            Self::Unpushed { branch, commits: 1 } => write!(f, "1 unpushed commit on {branch}"),
            Self::Unpushed { branch, commits } => {
                write!(f, "{commits} unpushed commits on {branch}")
            }
        }
    }
}

/// Everything in the worktree at `path` that removing it, and deleting
/// `branch` with it, would lose: changed tracked files, untracked files,
/// ignored files outside the reconstructible [`Presets`], and commits of
/// `HEAD` or `branch` that nothing else keeps. A commit is kept when a
/// remote-tracking branch, a tag or a local branch other than `branch`
/// reaches it.
///
/// # Errors
///
/// [`WorkspaceError::NotARepository`] if `path` is not a worktree,
/// [`WorkspaceError::Config`] if its project config is invalid, and
/// [`WorkspaceError::Git`] if Git fails.
pub fn unsaved_work(path: &Path, branch: Option<&str>) -> Result<Vec<UnsavedWork>, WorkspaceError> {
    if !path.join(".git").exists() {
        return Err(WorkspaceError::NotARepository {
            path: path.to_path_buf(),
        });
    }
    let presets = Presets::load(path)?;
    let mut found = changed_files(path, &presets)?;
    let mut head = stdout_line(path, &["branch", "--show-current"])?;
    if head.is_empty() {
        "HEAD".clone_into(&mut head);
    }
    let mut revs = vec![head];
    if let Some(branch) = branch
        && revs[0] != branch
        && branch_exists(path, branch)?
    {
        revs.push(branch.to_owned());
    }
    for rev in revs {
        let commits = unkept_commits(path, &rev, branch)?;
        if commits > 0 {
            found.push(UnsavedWork::Unpushed {
                branch: rev,
                commits,
            });
        }
    }
    Ok(found)
}

fn changed_files(path: &Path, presets: &Presets) -> Result<Vec<UnsavedWork>, WorkspaceError> {
    // `matching` lists an ignored directory once, not each file in it, and
    // `normal` does the same for an untracked directory.
    let out = stdout(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignored=matching",
        ],
    )?;
    let mut found = Vec::new();
    let mut entries = out.split(|&b| b == 0).filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        let (Some(status), Some(name)) = (entry.get(..2), entry.get(3..)) else {
            continue;
        };
        let file = PathBuf::from(OsStr::from_bytes(name));
        match status {
            b"??" => found.push(UnsavedWork::Untracked(file)),
            b"!!" => {
                if !presets.dirs().iter().any(|dir| file.starts_with(dir)) {
                    found.push(UnsavedWork::Ignored(file));
                }
            }
            _ => {
                // A rename or copy is followed by its source path.
                if status.contains(&b'R') || status.contains(&b'C') {
                    entries.next();
                }
                found.push(UnsavedWork::Uncommitted(file));
            }
        }
    }
    Ok(found)
}

fn unkept_commits(path: &Path, rev: &str, deleting: Option<&str>) -> Result<u32, WorkspaceError> {
    // Before `--branches`, the pattern is a branch name without
    // `refs/heads/` (git-rev-list(1)); ref names can't hold glob characters
    // (git-check-ref-format(1)), so it matches exactly that branch.
    let exclude = deleting.map(|branch| format!("--exclude={branch}"));
    let mut args = vec!["rev-list", "--count", rev, "--not"];
    if let Some(exclude) = &exclude {
        args.push(exclude);
    }
    args.extend(["--branches", "--remotes", "--tags"]);
    let count = stdout_line(path, &args)?;
    count.parse().map_err(|_| WorkspaceError::Git {
        command: args.join(" "),
        message: format!("expected a count, got {count:?}"),
    })
}
