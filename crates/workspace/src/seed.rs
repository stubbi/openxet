//! Seeding a new workspace's reconstructible directories with
//! copy-on-write clones of a sibling's, so its first build reuses the
//! sibling's outputs and its disk use grows only by what it changes.
//!
//! Clones only, never a byte copy: a copy would double the disk use this
//! exists to avoid, so without clone support seeding does nothing.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, FileTimes, TryLockError};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rustix::io::Errno;

use crate::{
    Class, Presets, ProjectId, Registry, WorkspaceError, classify, git, reconstructible_dirs,
};

/// A file modified this recently may still be being written by a build, so
/// it is not cloned; the new workspace's build makes it again.
const SETTLE: Duration = Duration::from_secs(2);

/// Python's virtual environments hold their own absolute path in scripts
/// and `pyvenv.cfg` and are "not considered as movable or copyable"
/// (<https://docs.python.org/3/library/venv.html>): a clone would run, and
/// install into, the sibling's.
const NOT_RELOCATABLE: &str = ".venv";

/// What [`seed`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedReport {
    /// The size of the files cloned. Clones share their blocks with the
    /// sibling's files, so this is not new disk use.
    pub cloned_bytes: u64,
    /// Why seeding stopped short, if it did.
    pub skipped: Option<SeedSkip>,
}

/// Why nothing, or not everything, was seeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedSkip {
    /// No other worktree of the project has reconstructible directories.
    NoSibling,
    /// The filesystem can't clone files, as on ext4 or a network volume.
    ReflinkUnsupported,
    /// A clone failed for another reason, given here; the directory it was
    /// cloning was removed again, so the build makes it from scratch.
    Failed(String),
}

impl SeedSkip {
    /// The stable name of this reason, as `openxet.workspace.created.v1`
    /// reports it.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoSibling => "no_sibling",
            Self::ReflinkUnsupported => "reflink_unsupported",
            Self::Failed(_) => "failed",
        }
    }
}

/// Clones the reconstructible directories of the worktree `from` into the
/// new worktree `target`, where they don't exist yet, keeping each file's
/// mtime and permissions so build tools see their outputs as fresh. Then
/// gives `target`'s tracked files the mtimes of `from`'s where their content
/// is the same, since Cargo would otherwise find every source newer than
/// its outputs and rebuild them. `from` of `None` reports
/// [`SeedSkip::NoSibling`].
#[must_use]
pub fn seed(target: &Path, from: Option<&Path>, presets: &Presets) -> SeedReport {
    seed_with(target, from, presets, &|from, to| {
        reflink_copy::reflink(from, to)
    })
}

/// [`seed`] with `clone` in place of the filesystem's clone call, so tests
/// can stand in for a filesystem without clone support, or without clones
/// at all.
#[doc(hidden)]
pub fn seed_with(
    target: &Path,
    from: Option<&Path>,
    presets: &Presets,
    clone: &dyn Fn(&Path, &Path) -> io::Result<()>,
) -> SeedReport {
    let mut report = SeedReport {
        cloned_bytes: 0,
        skipped: None,
    };
    let Some(from) = from else {
        report.skipped = Some(SeedSkip::NoSibling);
        return report;
    };
    let settled_before = SystemTime::now() - SETTLE;
    for dir in seedable_dirs(from, presets) {
        let Ok(relative) = dir.strip_prefix(from) else {
            continue;
        };
        let dest = target.join(relative);
        let ignored_here = classify(target, &relative.join("x"), presets)
            .is_ok_and(|class| class == Class::Reconstructible);
        if fs::symlink_metadata(&dest).is_ok() || !ignored_here {
            continue;
        }
        let mut tree = Tree {
            from,
            target,
            settled_before,
            clone,
            bytes: 0,
        };
        match tree.clone_dir(&dir, &dest) {
            Ok(()) => report.cloned_bytes += tree.bytes,
            Err(failure) => {
                // Only what this call made: `dest` did not exist before.
                let _ = fs::remove_dir_all(&dest);
                report.skipped = Some(failure);
                break;
            }
        }
    }
    if report.cloned_bytes > 0 {
        // Best effort: a file left with its checkout mtime costs only the
        // rebuild of the crate it belongs to.
        let _ = align_source_mtimes(target, from);
    }
    report
}

/// The longest [`wait_until_settled`] waits for in `create`. A finished
/// build settles within the 2 s settle window, so a longer wait means the sibling is
/// still building: 30 s covers the end of a build, such as linking a large
/// binary, without holding an agent for a whole build. Claude Code gives a
/// command hook such as `WorktreeCreate` 600 s by default
/// (<https://code.claude.com/docs/en/hooks>, "Common fields").
pub const SETTLE_CAP: Duration = Duration::from_secs(30);

const SETTLE_POLL: Duration = Duration::from_millis(250);

/// The files Cargo locks exclusively, in each profile's directory under
/// `target`, for the length of a build: `.cargo-lock` in earlier releases;
/// Cargo 1.97.1, as observed on the gate's fixture, holds
/// `.cargo-build-lock` and `.cargo-artifact-lock` instead and leaves
/// `.cargo-lock` free.
const CARGO_LOCKS: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];

/// What [`wait_until_settled`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettleWait {
    /// How long it waited.
    pub waited: Duration,
    /// False when the cap passed with files still being written or a Cargo
    /// build still running: seeding then clones only the settled files.
    pub settled: bool,
}

/// Waits, up to `cap`, until nothing under the reconstructible directories
/// of the worktree `from` was modified within the settle window and no
/// Cargo build holds a lock there, so a seed taken right after a
/// sibling's build gets all of its outputs rather than skipping the newest.
#[must_use]
pub fn wait_until_settled(from: &Path, presets: &Presets, cap: Duration) -> SettleWait {
    let started = Instant::now();
    loop {
        let settled = is_settled(from, presets);
        let waited = started.elapsed();
        if settled || waited >= cap {
            return SettleWait { waited, settled };
        }
        std::thread::sleep(SETTLE_POLL.min(cap.saturating_sub(waited)));
    }
}

fn is_settled(from: &Path, presets: &Presets) -> bool {
    let settled_before = SystemTime::now() - SETTLE;
    seedable_dirs(from, presets).iter().all(|dir| {
        let mut settled = true;
        crate::space::walk(dir, &mut |meta| {
            if meta.is_file() && meta.modified().is_ok_and(|at| at > settled_before) {
                settled = false;
            }
        });
        settled && !cargo_build_running(dir)
    })
}

/// Whether a Cargo build holds a lock under `dir`: `<dir>/<profile>` or
/// `<dir>/<target triple>/<profile>`.
fn cargo_build_running(dir: &Path) -> bool {
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path())
                    .collect()
            })
            .unwrap_or_default()
    };
    subdirs(dir)
        .into_iter()
        .flat_map(|profile| {
            let mut candidates = subdirs(&profile);
            candidates.push(profile);
            candidates
        })
        .any(|dir| {
            // A shared lock fails only while Cargo holds its exclusive one
            // (both are flock(2) locks on Unix).
            CARGO_LOCKS.iter().any(|name| {
                File::open(dir.join(name)).is_ok_and(|lock| {
                    matches!(lock.try_lock_shared(), Err(TryLockError::WouldBlock))
                })
            })
        })
}

/// The bytes [`seed`] would clone from `from`, for a dry run.
#[must_use]
pub fn seedable_bytes(from: &Path, presets: &Presets) -> u64 {
    let settled_before = SystemTime::now() - SETTLE;
    let mut total = 0u64;
    for dir in seedable_dirs(from, presets) {
        crate::space::walk(&dir, &mut |meta| {
            if meta.is_file() && meta.modified().is_ok_and(|at| at <= settled_before) {
                total = total.saturating_add(meta.len());
            }
        });
    }
    total
}

fn seedable_dirs(from: &Path, presets: &Presets) -> Vec<PathBuf> {
    reconstructible_dirs(from, presets)
        .into_iter()
        .filter(|dir| {
            dir.strip_prefix(from)
                .is_ok_and(|rel| rel != Path::new(NOT_RELOCATABLE))
        })
        .collect()
}

/// Whether files in `dir` can be cloned: clones a one-byte probe file
/// there, then removes both.
#[must_use]
pub fn reflink_supported(dir: &Path) -> bool {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let probe = dir.join(format!(
        ".openxet-clone-probe-{}-{nanos}",
        std::process::id()
    ));
    let mut clone_path = probe.clone().into_os_string();
    clone_path.push("-clone");
    let clone_path = PathBuf::from(clone_path);
    let supported =
        fs::write(&probe, [0u8]).is_ok() && reflink_copy::reflink(&probe, &clone_path).is_ok();
    let _ = fs::remove_file(&clone_path);
    let _ = fs::remove_file(&probe);
    supported
}

/// The worktree of `project` to seed `target` from: of those with
/// something to seed, the registered workspaces and the main worktree,
/// the one whose `HEAD` is the fewest commits from `base`
/// (`git rev-list --count base...HEAD`), and of those the most recently
/// active. The main worktree, which the registry doesn't track, loses
/// ties.
///
/// # Errors
///
/// [`WorkspaceError::Database`] if the registry can't be read and
/// [`WorkspaceError::Config`] if `target`'s project config is invalid.
pub async fn nearest_sibling(
    registry: &Registry,
    project: &ProjectId,
    target: &Path,
    base: &str,
) -> Result<Option<PathBuf>, WorkspaceError> {
    let presets = Presets::load(target)?;
    let mut candidates: Vec<(PathBuf, SystemTime)> = registry
        .list(Some(project))
        .await?
        .into_iter()
        .map(|workspace| (workspace.path, workspace.last_active))
        .collect();
    if let Ok(main) = git::main_worktree(Path::new(project.as_str()))
        .and_then(|main| main.canonicalize().map_err(WorkspaceError::io(main)))
        && !candidates.iter().any(|(path, _)| *path == main)
    {
        candidates.push((main, UNIX_EPOCH));
    }
    Ok(candidates
        .into_iter()
        .filter(|(path, _)| path != target && !seedable_dirs(path, &presets).is_empty())
        .filter_map(|(path, active)| {
            let head = git::head(&path)?;
            let distance = git::distance(&path, base, &head)?;
            Some((distance, Reverse(active), path))
        })
        .min_by_key(|(distance, active, _)| (*distance, *active))
        .map(|(_, _, path)| path))
}

/// One directory tree being cloned.
struct Tree<'a> {
    from: &'a Path,
    target: &'a Path,
    settled_before: SystemTime,
    clone: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
    bytes: u64,
}

impl Tree<'_> {
    fn clone_dir(&mut self, src: &Path, dest: &Path) -> Result<(), SeedSkip> {
        // Directories get their permissions after their contents, so a
        // read-only one can still be filled.
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(failed(parent))?;
        }
        let mut dirs = Vec::new();
        let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
        while let Some((src, dest)) = stack.pop() {
            let meta = fs::symlink_metadata(&src).map_err(failed(&src))?;
            let kind = meta.file_type();
            if kind.is_dir() {
                fs::create_dir(&dest).map_err(failed(&dest))?;
                for entry in fs::read_dir(&src).map_err(failed(&src))? {
                    let name = entry.map_err(failed(&src))?.file_name();
                    stack.push((src.join(&name), dest.join(&name)));
                }
                dirs.push((dest, meta.permissions()));
            } else if kind.is_symlink() {
                self.clone_symlink(&src, &dest)?;
            } else if kind.is_file() && meta.modified().is_ok_and(|at| at <= self.settled_before) {
                self.clone_file(&src, &dest, &meta)?;
            }
            // Sockets, FIFOs and devices are not build outputs to reuse.
        }
        for (dir, permissions) in dirs.into_iter().rev() {
            fs::set_permissions(&dir, permissions).map_err(failed(&dir))?;
        }
        Ok(())
    }

    fn clone_file(&mut self, src: &Path, dest: &Path, meta: &fs::Metadata) -> Result<(), SeedSkip> {
        (self.clone)(src, dest).map_err(|err| {
            if is_unsupported(&err) {
                SeedSkip::ReflinkUnsupported
            } else {
                failed(src)(err)
            }
        })?;
        let modified = meta.modified().map_err(failed(src))?;
        // Linux's FICLONE gives the new file the current time; Cargo
        // would see an output newer than it is and trust a stale one.
        File::open(dest)
            .and_then(|file| file.set_times(FileTimes::new().set_modified(modified)))
            .map_err(failed(dest))?;
        fs::set_permissions(dest, meta.permissions()).map_err(failed(dest))?;
        self.bytes = self.bytes.saturating_add(meta.len());
        Ok(())
    }

    /// Recreates a symbolic link; one pointing into the sibling by an
    /// absolute path is pointed at the same place in the new workspace, so
    /// the workspaces stay independent.
    fn clone_symlink(&self, src: &Path, dest: &Path) -> Result<(), SeedSkip> {
        let link = fs::read_link(src).map_err(failed(src))?;
        let link = match link.strip_prefix(self.from) {
            Ok(inside) if link.is_absolute() => self.target.join(inside),
            _ => link,
        };
        symlink(&link, dest).map_err(failed(dest))
    }
}

fn failed(path: &Path) -> impl FnOnce(io::Error) -> SeedSkip + '_ {
    move |err| SeedSkip::Failed(format!("{}: {err}", path.display()))
}

/// Whether a clone failed because the filesystem can't clone, rather than
/// for this one file.
fn is_unsupported(err: &io::Error) -> bool {
    if matches!(
        err.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::CrossesDevices
    ) {
        return true;
    }
    // `clonefile` answers ENOTSUP (clonefile(2)); `FICLONE` answers
    // EOPNOTSUPP, or EINVAL where the filesystem can't share these files'
    // blocks (ioctl_ficlone(2)).
    Errno::from_io_error(err).is_some_and(|errno| {
        errno == Errno::NOTSUP
            || errno == Errno::OPNOTSUPP
            || (cfg!(target_os = "linux") && errno == Errno::INVAL)
    })
}

/// Gives each tracked file of `target` the mtime of the same file in
/// `from`, where both hold the same content: the same blob in both indexes
/// and no unstaged change in `from`.
fn align_source_mtimes(target: &Path, from: &Path) -> Result<(), WorkspaceError> {
    let ours = index_entries(target)?;
    let theirs = index_entries(from)?;
    let dirty: HashSet<Vec<u8>> = git::stdout(from, &["diff", "--name-only", "-z"])?
        .split(|&b| b == 0)
        .map(<[u8]>::to_vec)
        .collect();
    for (path, entry) in &ours {
        if theirs.get(path) != Some(entry) || dirty.contains(path) {
            continue;
        }
        let relative = Path::new(OsStr::from_bytes(path));
        let Ok(meta) = fs::symlink_metadata(from.join(relative)) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        let dest = target.join(relative);
        if let (Ok(modified), Ok(file)) = (meta.modified(), File::open(&dest)) {
            let _ = file.set_times(FileTimes::new().set_modified(modified));
        }
    }
    Ok(())
}

/// The index of the worktree `dir`: path to mode and blob id.
fn index_entries(dir: &Path) -> Result<HashMap<Vec<u8>, Vec<u8>>, WorkspaceError> {
    let out = git::stdout(dir, &["ls-files", "--stage", "-z"])?;
    Ok(out
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let tab = entry.iter().position(|&b| b == b'\t')?;
            Some((entry[tab + 1..].to_vec(), entry[..tab].to_vec()))
        })
        .collect())
}
