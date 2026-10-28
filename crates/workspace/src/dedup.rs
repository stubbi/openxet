use std::fs::{self, File, FileTimes, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::{Registry, WorkspaceError};

/// Files smaller than this are left alone: a clone's own metadata and the
/// hash-and-swap work outweigh what sharing their blocks would save.
pub const MIN_DEDUP_SIZE: u64 = 64 << 10;

/// The name prefix of the temporary clone a swap renames over the file.
/// Dot-prefixed and OpenXet-named so no build tool writes or globs it; the
/// rest of the name is a fresh UUID, never derived from the file's name, so
/// it can't exceed the filesystem's name limit.
const TEMP_PREFIX: &str = ".openxet-dedup-";

/// What [`dedup_file`] did with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DedupOutcome {
    /// The file was replaced by a clone of the canonical copy of its
    /// content, so its `bytes` are shared with it. They are freed unless an
    /// earlier pass already cloned this file: POSIX has no call that says
    /// whether two files share blocks, so a clone is cloned again.
    Cloned {
        /// The file's size.
        bytes: u64,
    },
    /// The file is now, or already was, the copy others are cloned from.
    Canonical,
    /// The file was modified within the settle window, so it may still be
    /// being written.
    Unsettled,
    /// The file changed while it was being deduplicated and was left as it
    /// is.
    Changed,
    /// The file is under [`MIN_DEDUP_SIZE`].
    TooSmall,
    /// The path is not a regular file (a symlink, a directory or another
    /// kind) and is never deduplicated.
    NotAFile,
    /// The filesystem can't clone files. Nothing was copied.
    Unsupported,
}

/// What [`dedup_tree`] did under one directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DedupReport {
    /// Bytes of the files replaced by clones: what was freed, at most, since
    /// a file cloned on an earlier pass counts again.
    pub cloned_bytes: u64,
    /// Regular files visited.
    pub files_seen: u64,
    /// Files that could not be deduplicated because of an error, such as a
    /// file deleted mid-walk.
    pub failed: u64,
    /// The filesystem can't clone files; the walk stopped at the first file
    /// that showed it.
    pub unsupported: bool,
}

/// Deduplicates one settled file: if a canonical copy of the same content
/// exists, replaces `path` by a copy-on-write clone of it, keeping `path`'s
/// timestamps and mode so build tools still see it as fresh; otherwise
/// records `path` as the canonical copy.
///
/// `path` must be canonical, as the registry compares paths byte for byte.
/// Symlinks are never followed. A file modified less than `settle` ago, or
/// changed at any point before the swap, is left untouched.
///
/// # Errors
///
/// [`WorkspaceError::Io`] if a file can't be read or the swap fails, and
/// [`WorkspaceError::Database`] if the dedup index can't be read or written.
pub async fn dedup_file(
    reg: &Registry,
    path: &Path,
    settle: Duration,
) -> Result<DedupOutcome, WorkspaceError> {
    dedup_file_with(reg, path, settle, |_| {}).await
}

/// [`dedup_file`] with `before_swap` called once the clone is ready and
/// right before the file is re-checked and replaced, so a test can change
/// the file in that window.
async fn dedup_file_with(
    reg: &Registry,
    path: &Path,
    settle: Duration,
    before_swap: impl FnOnce(&Path),
) -> Result<DedupOutcome, WorkspaceError> {
    let before = fs::symlink_metadata(path).map_err(WorkspaceError::io(path))?;
    if !before.file_type().is_file() {
        return Ok(DedupOutcome::NotAFile);
    }
    let size = before.len();
    if size < MIN_DEDUP_SIZE {
        return Ok(DedupOutcome::TooSmall);
    }
    let modified = before.modified().map_err(WorkspaceError::io(path))?;
    // A future mtime (clock skew, or a tool setting it) errs on the side of
    // waiting.
    let age = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO);
    if age < settle {
        return Ok(DedupOutcome::Unsettled);
    }

    let hash = hash_file(path.to_path_buf()).await?;
    let Some(canon) = reg.canonical(size, hash).await? else {
        reg.set_canonical(size, hash, path).await?;
        return Ok(DedupOutcome::Canonical);
    };
    if canon == path {
        return Ok(DedupOutcome::Canonical);
    }
    let canon_meta = match fs::symlink_metadata(&canon) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return replace_canonical(reg, size, hash, path).await;
        }
        Err(err) => return Err(WorkspaceError::io(canon)(err)),
    };
    if canon_meta.dev() == before.dev() && canon_meta.ino() == before.ino() {
        // A hard link of the canonical copy: its blocks are shared already.
        return Ok(DedupOutcome::Canonical);
    }
    // A canonical copy on another volume can't be cloned from, so this file
    // starts this volume's.
    if !canon_meta.file_type().is_file()
        || canon_meta.len() != size
        || canon_meta.dev() != before.dev()
    {
        return replace_canonical(reg, size, hash, path).await;
    }

    let parent = path.parent().ok_or_else(|| WorkspaceError::Io {
        path: path.to_path_buf(),
        source: io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path has no parent directory",
        ),
    })?;
    let temp = TempFile(parent.join(format!("{TEMP_PREFIX}{}", uuid::Uuid::now_v7().simple())));
    match reflink_copy::reflink(&canon, &temp.0) {
        Ok(()) => {}
        Err(err) if is_unsupported(&err) => return Ok(DedupOutcome::Unsupported),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return replace_canonical(reg, size, hash, path).await;
        }
        Err(err) => return Err(WorkspaceError::io(path)(err)),
    }
    // The clone, not the canonical copy, is hashed: the clone's blocks are
    // what lands at `path`, and a canonical copy rewritten after any earlier
    // check can't slip through.
    if hash_file(temp.0.clone()).await? != hash {
        return replace_canonical(reg, size, hash, path).await;
    }
    keep_times_and_mode(&temp.0, &before)?;

    before_swap(path);
    let now = fs::symlink_metadata(path).map_err(WorkspaceError::io(path))?;
    if !same_file_unchanged(&before, &now) {
        return Ok(DedupOutcome::Changed);
    }
    // A writer that opens `path` between this check and the rename, or held
    // it open through the settle window, writes into the replaced inode. The
    // window is two syscalls wide, and what it can lose is one write to a
    // reconstructible file, which the next build redoes.
    fs::rename(&temp.0, path).map_err(WorkspaceError::io(path))?;
    temp.keep();
    Ok(DedupOutcome::Cloned { bytes: size })
}

/// Deduplicates every regular file under `root` with [`dedup_file`],
/// without following symlinks or crossing into other filesystems.
pub async fn dedup_tree(reg: &Registry, root: &Path, settle: Duration) -> DedupReport {
    let mut report = DedupReport::default();
    let root = root.to_path_buf();
    let Ok(Ok((files, unreadable))) =
        tokio::task::spawn_blocking(move || regular_files(&root)).await
    else {
        report.failed = 1;
        return report;
    };
    report.failed = unreadable;
    for file in files {
        report.files_seen += 1;
        match dedup_file(reg, &file, settle).await {
            Ok(DedupOutcome::Cloned { bytes }) => report.cloned_bytes += bytes,
            Ok(DedupOutcome::Unsupported) => {
                report.unsupported = true;
                break;
            }
            Ok(_) => {}
            Err(_) => report.failed += 1,
        }
    }
    report
}

/// The regular files under `root` on `root`'s filesystem, and how many
/// directories or entries could not be read.
fn regular_files(root: &Path) -> io::Result<(Vec<PathBuf>, u64)> {
    let device = fs::symlink_metadata(root)?.dev();
    let mut files = Vec::new();
    let mut unreadable = 0;
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            unreadable += 1;
            continue;
        };
        for entry in entries {
            let Ok(entry) = entry else {
                unreadable += 1;
                continue;
            };
            // `DirEntry::file_type` doesn't follow symlinks.
            let Ok(kind) = entry.file_type() else {
                unreadable += 1;
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                if entry.metadata().is_ok_and(|meta| meta.dev() == device) {
                    dirs.push(path);
                }
            } else if kind.is_file()
                && !entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(TEMP_PREFIX.as_bytes())
            {
                files.push(path);
            }
        }
    }
    Ok((files, unreadable))
}

async fn replace_canonical(
    reg: &Registry,
    size: u64,
    hash: [u8; 32],
    path: &Path,
) -> Result<DedupOutcome, WorkspaceError> {
    reg.set_canonical(size, hash, path).await?;
    Ok(DedupOutcome::Canonical)
}

async fn hash_file(path: PathBuf) -> Result<[u8; 32], WorkspaceError> {
    tokio::task::spawn_blocking(move || {
        let mut hasher = blake3::Hasher::new();
        File::open(&path)
            .and_then(|file| hasher.update_reader(file).map(|_| ()))
            .map_err(WorkspaceError::io(&path))?;
        Ok(*hasher.finalize().as_bytes())
    })
    .await
    .unwrap_or_else(|err| std::panic::resume_unwind(err.into_panic()))
}

/// Gives the clone `original`'s access and modification times and mode:
/// Cargo, Make and Ninja compare mtimes, so a swapped file with a new mtime
/// would trigger a rebuild, and a clone otherwise carries the canonical
/// copy's mode.
fn keep_times_and_mode(clone: &Path, original: &Metadata) -> Result<(), WorkspaceError> {
    let accessed = original.accessed().map_err(WorkspaceError::io(clone))?;
    let modified = original.modified().map_err(WorkspaceError::io(clone))?;
    // Read-only: the canonical copy may not be writable, and the owner may
    // set times and mode without write access.
    let file = File::open(clone).map_err(WorkspaceError::io(clone))?;
    file.set_times(
        FileTimes::new()
            .set_accessed(accessed)
            .set_modified(modified),
    )
    .map_err(WorkspaceError::io(clone))?;
    file.set_permissions(original.permissions())
        .map_err(WorkspaceError::io(clone))
}

/// Whether `now` is the same inode as `before`, with no change since: the
/// size, mtime and ctime catch a rewrite in place (ctime also catches one
/// that restores the mtime), the device and inode a rename over it.
fn same_file_unchanged(before: &Metadata, now: &Metadata) -> bool {
    now.file_type().is_file()
        && now.dev() == before.dev()
        && now.ino() == before.ino()
        && now.len() == before.len()
        && now.modified().ok() == before.modified().ok()
        && (now.ctime(), now.ctime_nsec()) == (before.ctime(), before.ctime_nsec())
}

/// Whether a clone failed because the filesystem (or the pair of paths)
/// can't share blocks, rather than for an I/O reason worth reporting:
/// `clonefile(2)` answers `ENOTSUP` off APFS, and `FICLONE`
/// (`ioctl_ficlone(2)`) answers `EOPNOTSUPP`, `EXDEV`, `EINVAL` or `ENOTTY`
/// depending on the filesystem and kernel.
fn is_unsupported(err: &io::Error) -> bool {
    if err.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    let codes = [
        libc::ENOTSUP,
        libc::EOPNOTSUPP,
        libc::EXDEV,
        libc::ENOTTY,
        #[cfg(target_os = "linux")]
        libc::EINVAL,
    ];
    err.raw_os_error().is_some_and(|code| codes.contains(&code))
}

/// A temporary path removed on drop unless kept, so every early return and
/// error leaves no clone behind.
struct TempFile(PathBuf);

impl TempFile {
    fn keep(self) {
        std::mem::forget(self);
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::fs::{self, File, FileTimes};
    use std::os::unix::fs::MetadataExt as _;
    use std::time::{Duration, UNIX_EPOCH};

    use super::{DedupOutcome, TEMP_PREFIX, dedup_file, dedup_file_with};
    use crate::Registry;

    fn settled(path: &std::path::Path, bytes: &[u8]) {
        fs::write(path, bytes).unwrap();
        let old = UNIX_EPOCH + Duration::new(1_761_600_000, 0);
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(FileTimes::new().set_accessed(old).set_modified(old))
            .unwrap();
    }

    #[tokio::test]
    async fn file_changed_after_hashing_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let state = tempfile::tempdir().unwrap();
        let reg = Registry::open(state.path()).await.unwrap();
        let bytes = vec![7u8; 256 << 10];
        let rewritten = vec![9u8; 256 << 10];
        let canon = dir.join("canon");
        let copy = dir.join("copy");
        let renamed = dir.join("renamed");
        settled(&canon, &bytes);
        settled(&copy, &bytes);
        settled(&renamed, &bytes);
        // macOS's temporary directory is always on APFS, so this never skips
        // there; a Linux runner's ext4 or tmpfs has no clones.
        if reflink_copy::reflink(&canon, dir.join("probe")).is_err() {
            assert_ne!(
                std::env::consts::OS,
                "macos",
                "{} can't clone files",
                dir.display()
            );
            eprintln!("SKIPPED file_changed_after_hashing_is_left_alone: no clones");
            return;
        }
        fs::remove_file(dir.join("probe")).unwrap();
        let settle = Duration::from_secs(2);
        assert_eq!(
            dedup_file(&reg, &canon, settle).await.unwrap(),
            DedupOutcome::Canonical
        );

        // Rewritten in place with same-size bytes, as a build step would.
        let outcome = dedup_file_with(&reg, &copy, settle, |path| {
            fs::write(path, &rewritten).unwrap();
        })
        .await
        .unwrap();
        assert_eq!(outcome, DedupOutcome::Changed);
        assert_eq!(fs::read(&copy).unwrap(), rewritten);

        // Replaced by rename with the original mtime kept, as `cp -p` or a
        // linker writing a temporary file would.
        let outcome = dedup_file_with(&reg, &renamed, settle, |path| {
            let fresh = path.with_extension("new");
            settled(&fresh, &rewritten);
            fs::rename(&fresh, path).unwrap();
        })
        .await
        .unwrap();
        assert_eq!(outcome, DedupOutcome::Changed);
        assert_eq!(fs::read(&renamed).unwrap(), rewritten);
        assert_ne!(
            fs::metadata(&renamed).unwrap().ino(),
            fs::metadata(&canon).unwrap().ino()
        );

        for entry in fs::read_dir(&dir).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(
                !name.to_string_lossy().starts_with(TEMP_PREFIX),
                "temporary file {name:?} left behind"
            );
        }
    }
}
