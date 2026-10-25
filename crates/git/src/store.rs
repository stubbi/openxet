//! The node cache (ADR 0004 item 4): hydrated packs under
//! `{cache_dir}/repos/{repo id}/`, request snapshots under
//! `{cache_dir}/snapshots/`, openxet's hook directory and git's empty
//! `HOME`. Every path below the root comes from a repository id or a
//! checked pack name, never from a repository's name (item 7).

use std::collections::BTreeMap;
use std::fs::{self, File, FileTimes, OpenOptions, TryLockError};
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_index::RepoId;
use openxet_storage::{
    Bucket, Key, MidxExt, PackExt, PackName, RepoIdHex, StorageError, create_private_dir_all,
};
use tokio::sync::{RwLock, RwLockReadGuard};

use crate::{GitError, blocking, io_err};

const REPOS: &str = "repos";
const SNAPSHOTS: &str = "snapshots";
const HOOKS: &str = "hooks";
const HOME: &str = "home";
const LOCK: &str = "lock";
const MIDX: &str = "midx";

/// A pack's files in hydration order. The `.pack` comes last, so its
/// presence means every file the bucket holds for it is cached: eviction
/// leaves a unit without it alone (a hydration in progress), removes it
/// first, and start-up sweeps units a crash left without it.
const PACK_HYDRATION_ORDER: [PackExt; 5] = [
    PackExt::Idx,
    PackExt::Rev,
    PackExt::Bitmap,
    PackExt::Mtimes,
    PackExt::Pack,
];

/// A MIDX generation's files in hydration order; the index itself last, as
/// for packs.
const MIDX_HYDRATION_ORDER: [MidxExt; 3] = [MidxExt::Bitmap, MidxExt::Rev, MidxExt::MultiPackIndex];

/// What an eviction did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Eviction {
    /// Bytes removed from the cache: files no snapshot linked.
    pub freed_bytes: u64,
    /// How far the cache is still over the budget: space only packs that
    /// snapshots link hold, which returns when those requests end and a
    /// later eviction removes them.
    pub deficit_bytes: u64,
}

/// `EXDEV` and `EPERM`, the same numbers on Linux and macOS: a probe link
/// failing with them means the filesystem layout is wrong (`EPERM` is how
/// Linux reports a filesystem without hard links), while `EACCES` and the
/// rest are permissions or I/O.
const EXDEV: i32 = 18;
const EPERM: i32 = 1;

/// The node cache of one process.
#[derive(Debug)]
pub(crate) struct NodeCache {
    root: PathBuf,
    /// Linking a snapshot holds it shared; eviction holds it exclusively,
    /// so it never removes a pack between the final hydration check and the
    /// link. Downloads run outside it, so a queued eviction never waits on
    /// a slow hydration.
    files: RwLock<()>,
    /// Held locked while the process runs: start-up sweeps every snapshot
    /// directory, which is only safe if no other process is using them.
    _owner: File,
}

impl NodeCache {
    /// Opens the cache at `dir`, creating it: takes the process lock, sweeps
    /// leaked snapshots, temp files and half-hydrated packs, recreates the
    /// hook directory and `HOME` empty, and checks that snapshots can
    /// hard-link cached packs (ADR 0004 item 4). The path must be UTF-8: it
    /// is written into every snapshot's config and git's `PATH`.
    pub(crate) fn open(dir: &Path) -> Result<Self, GitError> {
        if dir.to_str().is_none() {
            return Err(GitError::CacheDirNotUtf8 {
                path: dir.to_path_buf(),
            });
        }
        create_private_dir_all(dir).map_err(io_err("create the Git cache", dir))?;
        let root = fs::canonicalize(dir).map_err(io_err("resolve the Git cache", dir))?;
        if root.to_str().is_none() {
            return Err(GitError::CacheDirNotUtf8 { path: root });
        }
        let lock_path = root.join(LOCK);
        let owner = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .mode(0o600)
            .open(&lock_path)
            .map_err(io_err("create", &lock_path))?;
        match owner.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => return Err(GitError::CacheInUse { path: root }),
            Err(TryLockError::Error(e)) => return Err(io_err("lock", &lock_path)(e)),
        }
        let cache = Self {
            root,
            files: RwLock::new(()),
            _owner: owner,
        };
        for dir in [cache.repos(), cache.snapshots()] {
            create_private_dir_all(&dir).map_err(io_err("create", &dir))?;
        }
        empty_dir(&cache.snapshots())?;
        for dir in [cache.hooks(), cache.home()] {
            remove_any(&dir)?;
            create_private_dir_all(&dir).map_err(io_err("create", &dir))?;
        }
        sweep_leftovers(&cache.repos())?;
        cache.check_links()?;
        Ok(cache)
    }

    pub(crate) fn snapshots(&self) -> PathBuf {
        self.root.join(SNAPSHOTS)
    }

    pub(crate) fn hooks(&self) -> PathBuf {
        self.root.join(HOOKS)
    }

    pub(crate) fn home(&self) -> PathBuf {
        self.root.join(HOME)
    }

    fn repos(&self) -> PathBuf {
        self.root.join(REPOS)
    }

    /// Where `repo`'s packs are cached: named by its id only.
    pub(crate) fn repo_dir(&self, repo: RepoId) -> PathBuf {
        self.repos().join(repo.to_string())
    }

    /// Where `repo`'s MIDX generations are cached.
    pub(crate) fn midx_dir(&self, repo: RepoId) -> PathBuf {
        self.repo_dir(repo).join(MIDX)
    }

    /// Hard links can't cross filesystems (`EXDEV`), so the snapshot root
    /// must share the pack cache's: compare `st_dev`, then make a probe
    /// link, which also catches filesystems without hard links.
    fn check_links(&self) -> Result<(), GitError> {
        let (repos, snapshots) = (self.repos(), self.snapshots());
        let refuse = |reason: String| GitError::CacheFilesystem {
            cache: repos.clone(),
            snapshots: snapshots.clone(),
            reason,
        };
        let dev = |p: &Path| fs::metadata(p).map(|m| m.dev()).map_err(io_err("stat", p));
        if dev(&repos)? != dev(&snapshots)? {
            return Err(refuse(
                "they are on different filesystems (st_dev differs)".to_owned(),
            ));
        }
        let name = format!(".probe-{}", uuid::Uuid::new_v4().simple());
        let (from, to) = (repos.join(&name), snapshots.join(&name));
        File::create(&from).map_err(io_err("create", &from))?;
        let linked = fs::hard_link(&from, &to);
        let _ = fs::remove_file(&to);
        fs::remove_file(&from).map_err(io_err("remove", &from))?;
        match linked {
            Ok(()) => Ok(()),
            Err(e) if matches!(e.raw_os_error(), Some(EXDEV | EPERM)) => {
                Err(refuse(format!("the probe link failed: {e}")))
            }
            Err(e) => Err(io_err("make a probe hard link into", &snapshots)(e)),
        }
    }

    /// Shared access for the final hydration check and the link.
    pub(crate) async fn read(&self) -> RwLockReadGuard<'_, ()> {
        self.files.read().await
    }

    /// Makes sure pack `name` of `repo` is cached with every file the bucket
    /// holds for it, its mtimes pinned to `created_at` (ADR 0004 item 4), and
    /// returns the bytes downloaded. A cached pack whose mtime moved, by a
    /// crash before the pin or by git freshening an object it holds, is
    /// pinned again. Called once without the lock to download, then under
    /// [`NodeCache::read`], where it only stats unless eviction removed the
    /// pack in between.
    pub(crate) async fn hydrate_pack(
        &self,
        bucket: &dyn Bucket,
        repo: RepoId,
        name: &PackName,
        created_at: u64,
    ) -> Result<u64, GitError> {
        let dir = self.repo_dir(repo);
        let path = |ext: PackExt| dir.join(format!("{name}.{}", ext.as_str()));
        let mtime = UNIX_EPOCH + Duration::from_secs(created_at);
        let pack = path(PackExt::Pack);
        match fs::metadata(&pack) {
            Ok(meta) if path(PackExt::Idx).exists() => {
                if meta.modified().ok() != Some(mtime) {
                    let files: Vec<PathBuf> =
                        PACK_HYDRATION_ORDER.iter().map(|e| path(*e)).collect();
                    blocking(move || files.iter().try_for_each(|f| pin(f, mtime))).await?;
                }
                return Ok(0);
            }
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("stat", &pack)(e)),
        }
        let repo_hex = RepoIdHex::parse(&repo.to_string())?;
        let mut bytes = 0;
        for ext in PACK_HYDRATION_ORDER {
            let file = path(ext);
            let key = Key::Pack {
                repo: repo_hex,
                name: name.clone(),
                ext,
            };
            match download(bucket, key, &dir, &file).await? {
                Some(n) => bytes += n,
                None if matches!(ext, PackExt::Pack | PackExt::Idx) => {
                    return Err(StorageError::NotFound {
                        key: Box::new(Key::Pack {
                            repo: repo_hex,
                            name: name.clone(),
                            ext,
                        }),
                    }
                    .into());
                }
                None => continue,
            }
            blocking(move || pin(&file, mtime)).await?;
        }
        Ok(bytes)
    }

    /// Makes sure MIDX generation `generation` of `repo` is cached with its
    /// bitmap and reverse index, if the bucket holds it, and returns the
    /// bytes downloaded. A generation missing from the bucket only costs
    /// speed: the snapshot then serves its packs without it. Called like
    /// [`NodeCache::hydrate_pack`].
    pub(crate) async fn hydrate_midx(
        &self,
        bucket: &dyn Bucket,
        repo: RepoId,
        generation: u64,
    ) -> Result<u64, GitError> {
        let dir = self.midx_dir(repo);
        let path = |ext: MidxExt| midx_file(&dir, generation, ext);
        if path(MidxExt::MultiPackIndex).exists() {
            return Ok(0);
        }
        let repo_hex = RepoIdHex::parse(&repo.to_string())?;
        let mut bytes = 0;
        for ext in MIDX_HYDRATION_ORDER {
            let key = Key::Midx {
                repo: repo_hex,
                generation,
                ext,
            };
            bytes += download(bucket, key, &dir, &path(ext)).await?.unwrap_or(0);
        }
        Ok(bytes)
    }

    /// Removes whole cached packs and MIDX generations that no snapshot
    /// links (`st_nlink == 1`), oldest first, until the cache holds at most
    /// `max_bytes` (ADR 0004 item 4). A linked file can't free space until
    /// its requests end, so it stays, and whatever it keeps over the budget
    /// is reported as the deficit; superseded packs leave once their links
    /// drop. Directories left empty are removed.
    pub(crate) async fn evict(&self, max_bytes: u64) -> Result<Eviction, GitError> {
        let _exclusive = self.files.write().await;
        let repos = self.repos();
        blocking(move || evict_blocking(&repos, max_bytes)).await
    }
}

/// Copies `key` to `file` in `dir`, creating `dir`; `None` if the bucket
/// lacks it. Downloads run outside the cache lock, so an eviction may remove
/// `dir` once it is empty, between its creation and the copy's temp file:
/// that is retried once.
async fn download(
    bucket: &dyn Bucket,
    key: Key,
    dir: &Path,
    file: &Path,
) -> Result<Option<u64>, GitError> {
    for attempt in 0..2 {
        create_private_dir_all(dir).map_err(io_err("create", dir))?;
        match bucket.get_to_file(key.clone(), file).await {
            Ok(n) => return Ok(Some(n)),
            Err(StorageError::NotFound { .. }) => return Ok(None),
            Err(StorageError::Io { .. }) if attempt == 0 && !dir.exists() => {}
            Err(e) => return Err(e.into()),
        }
    }
    Err(GitError::Io {
        op: "hydrate into",
        path: dir.to_path_buf(),
        source: std::io::Error::new(
            ErrorKind::NotFound,
            "the directory kept disappearing during the download",
        ),
    })
}

pub(crate) fn midx_file(dir: &Path, generation: u64, ext: MidxExt) -> PathBuf {
    dir.join(format!("{generation}.{}", ext.as_str()))
}

/// Pins `file`'s mtime and makes it read-only, and readable by this user
/// only (the data directory is private): a snapshot's git shares the
/// inode, and must never change a cached file in place. A file that is gone
/// (evicted while unlocked, or a side file the pack lacks) is skipped; the
/// check under the lock hydrates again.
pub(crate) fn pin(file: &Path, mtime: SystemTime) -> Result<(), GitError> {
    let f = match File::open(file) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err("open", file)(e)),
    };
    f.set_times(FileTimes::new().set_modified(mtime))
        .map_err(io_err("set the mtime of", file))?;
    fs::set_permissions(file, fs::Permissions::from_mode(0o400))
        .map_err(io_err("make read-only", file))
}

/// Removes everything in `dir`, keeping `dir`.
fn empty_dir(dir: &Path) -> Result<(), GitError> {
    for entry in fs::read_dir(dir).map_err(io_err("list", dir))? {
        remove_any(&entry.map_err(io_err("list", dir))?.path())?;
    }
    Ok(())
}

/// Removes `path`, a file or a directory tree (a symlink itself, never its
/// target), if it exists.
fn remove_any(path: &Path) -> Result<(), GitError> {
    let removed = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => Err(e),
    };
    removed.map_err(io_err("remove", path))
}

/// The cache's files, one level per repository plus its `midx/`.
fn cache_dirs(repos: &Path) -> Result<Vec<PathBuf>, GitError> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(repos).map_err(io_err("list", repos))? {
        let repo = entry.map_err(io_err("list", repos))?.path();
        if repo.is_dir() {
            dirs.push(repo.join(MIDX));
            dirs.push(repo);
        }
    }
    Ok(dirs.into_iter().filter(|d| d.is_dir()).collect())
}

/// Removes what a crash left behind: the temp files of interrupted
/// downloads (the bucket writes `.{name}.{uuid}.tmp` beside the file it
/// hydrates), the start-up probe's file, and the side files of packs and
/// MIDX generations whose hydration never reached its last file.
fn sweep_leftovers(repos: &Path) -> Result<(), GitError> {
    for entry in fs::read_dir(repos).map_err(io_err("list", repos))? {
        let path = entry.map_err(io_err("list", repos))?.path();
        if is_temp(&path) {
            remove_any(&path)?;
        }
    }
    for unit in scan(repos)?.into_values() {
        for (path, _) in &unit.files {
            if !unit.complete || is_temp(path) {
                remove_any(path)?;
            }
        }
    }
    Ok(())
}

fn is_temp(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// The files of one pack or MIDX generation, evicted together: git can't
/// use a pack without its index.
struct Unit {
    /// The `.pack` or `.multi-pack-index` first, so a half-evicted unit
    /// reads as not cached. Temp files come last.
    files: Vec<(PathBuf, u64)>,
    /// Whether that first file is there: a unit without it is still being
    /// hydrated, or a crash interrupted it.
    complete: bool,
    linked: bool,
    mtime: SystemTime,
}

/// Every file in the cache, by unit.
fn scan(repos: &Path) -> Result<BTreeMap<PathBuf, Unit>, GitError> {
    let mut units: BTreeMap<PathBuf, Unit> = BTreeMap::new();
    for dir in cache_dirs(repos)? {
        for entry in fs::read_dir(&dir).map_err(io_err("list", &dir))? {
            let path = entry.map_err(io_err("list", &dir))?.path();
            let meta = fs::symlink_metadata(&path).map_err(io_err("stat", &path))?;
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            // A temp file `.{name}.{uuid}.tmp` belongs to `name`'s unit, so
            // a download in progress keeps its unit incomplete.
            let base = name.strip_prefix('.').unwrap_or(name);
            let (stem, ext) = base.split_once('.').unwrap_or((base, ""));
            let unit = units.entry(dir.join(stem)).or_insert_with(|| Unit {
                files: Vec::new(),
                complete: false,
                linked: false,
                mtime: SystemTime::now(),
            });
            unit.linked |= meta.nlink() > 1;
            if let Ok(m) = meta.modified() {
                unit.mtime = unit.mtime.min(m);
            }
            let entry = (path.clone(), meta.len());
            let marker = ext == PackExt::Pack.as_str() || ext == MidxExt::MultiPackIndex.as_str();
            if marker && !is_temp(&path) {
                unit.complete = true;
                unit.files.insert(0, entry);
            } else {
                unit.files.push(entry);
            }
        }
    }
    Ok(units)
}

fn evict_blocking(repos: &Path, max_bytes: u64) -> Result<Eviction, GitError> {
    let units = scan(repos)?;
    let mut usage: u64 = units
        .values()
        .flat_map(|u| u.files.iter().map(|(_, len)| len))
        .sum();
    let mut candidates: Vec<Unit> = units
        .into_values()
        .filter(|u| u.complete && !u.linked && !u.files.iter().any(|(p, _)| is_temp(p)))
        .collect();
    candidates.sort_by_key(|u| u.mtime);
    let mut freed = 0;
    for unit in candidates {
        if usage <= max_bytes {
            break;
        }
        // No new link can appear while eviction holds the lock exclusively,
        // so a unit unlinked at the scan is still unlinked.
        for (path, len) in &unit.files {
            match fs::remove_file(path) {
                Ok(()) => {
                    freed += len;
                    usage -= len;
                }
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(io_err("remove", path)(e)),
            }
        }
    }
    remove_empty_dirs(repos)?;
    Ok(Eviction {
        freed_bytes: freed,
        deficit_bytes: usage.saturating_sub(max_bytes),
    })
}

/// Removes `midx/` and repository directories eviction left empty.
fn remove_empty_dirs(repos: &Path) -> Result<(), GitError> {
    for entry in fs::read_dir(repos).map_err(io_err("list", repos))? {
        let repo = entry.map_err(io_err("list", repos))?.path();
        if !fs::symlink_metadata(&repo).is_ok_and(|m| m.is_dir()) {
            continue;
        }
        for dir in [repo.join(MIDX), repo] {
            match fs::remove_dir(&dir) {
                Ok(()) => {}
                Err(e)
                    if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty) => {}
                Err(e) => return Err(io_err("remove", &dir)(e)),
            }
        }
    }
    Ok(())
}
