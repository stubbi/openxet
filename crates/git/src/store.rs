//! The node cache (ADR 0004 item 4): hydrated packs under
//! `{cache_dir}/repos/{repo id}/`, request snapshots under
//! `{cache_dir}/snapshots/`, openxet's hook directory and git's empty
//! `HOME`. Every path below the root comes from a repository id or a
//! checked pack name, never from a repository's name (item 7).

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, FileTimes, TryLockError};
use std::io::ErrorKind;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_index::RepoId;
use openxet_storage::{
    Bucket, Key, MidxExt, PackExt, PackName, RepoIdHex, StorageError, create_private_dir_all,
};
use tokio::sync::{RwLock, RwLockReadGuard};

use crate::snapshot::Layout;
use crate::{GitError, blocking, io_err};

const REPOS: &str = "repos";
const SNAPSHOTS: &str = "snapshots";
const HOOKS: &str = "hooks";
const HOME: &str = "home";
const LOCK: &str = "lock";
const MIDX: &str = "midx";

/// A pack's files in hydration order. The `.pack` comes last, so its
/// presence means every file the bucket holds for it is cached; eviction
/// removes it first for the same reason.
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
    /// Bytes of files removed from the cache that no snapshot linked: space
    /// returned now.
    pub freed_bytes: u64,
    /// Bytes of files removed from the cache that a snapshot still linked:
    /// returned only when those requests end, so not counted as freed.
    pub held_bytes: u64,
}

/// The node cache of one process.
#[derive(Debug)]
pub(crate) struct NodeCache {
    root: PathBuf,
    /// Hydration and snapshot building hold it shared; eviction holds it
    /// exclusively, so it never removes a pack between its hydration and
    /// its link into a snapshot.
    files: RwLock<()>,
    /// Held locked while the process runs: start-up sweeps every snapshot
    /// directory, which is only safe if no other process is using them.
    _owner: File,
}

impl NodeCache {
    /// Opens the cache at `dir`, creating it: takes the process lock, sweeps
    /// leaked snapshots and temp files, recreates the hook directory and
    /// `HOME` empty, and checks that snapshots can hard-link cached packs
    /// (ADR 0004 item 4).
    pub(crate) fn open(dir: &Path) -> Result<Self, GitError> {
        create_private_dir_all(dir).map_err(io_err("create the Git cache", dir))?;
        let root = fs::canonicalize(dir).map_err(io_err("resolve the Git cache", dir))?;
        let lock_path = root.join(LOCK);
        let owner = File::create(&lock_path).map_err(io_err("create", &lock_path))?;
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
        sweep_temp_files(&cache.repos())?;
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
        linked.map_err(|e| refuse(format!("the probe link failed: {e}")))
    }

    /// Shared access for hydrating and linking.
    pub(crate) async fn read(&self) -> RwLockReadGuard<'_, ()> {
        self.files.read().await
    }

    /// Makes sure pack `name` of `repo` is cached with every file the bucket
    /// holds for it, its mtimes pinned to `created_at` (ADR 0004 item 4), and
    /// returns the bytes downloaded. A cached pack whose mtime moved, by a
    /// crash before the pin or by git freshening an object it holds, is
    /// pinned again. The caller holds [`NodeCache::read`].
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
            Ok(meta) => {
                if meta.modified().ok() != Some(mtime) {
                    let files: Vec<PathBuf> =
                        PACK_HYDRATION_ORDER.iter().map(|e| path(*e)).collect();
                    blocking(move || {
                        files
                            .iter()
                            .filter(|f| f.exists())
                            .try_for_each(|f| pin(f, mtime))
                    })
                    .await?;
                }
                return Ok(0);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("stat", &pack)(e)),
        }
        create_private_dir_all(&dir).map_err(io_err("create", &dir))?;
        let repo_hex = RepoIdHex::parse(&repo.to_string())?;
        let mut bytes = 0;
        for ext in PACK_HYDRATION_ORDER {
            let file = path(ext);
            let key = Key::Pack {
                repo: repo_hex,
                name: name.clone(),
                ext,
            };
            match bucket.get_to_file(key, &file).await {
                Ok(n) => bytes += n,
                Err(StorageError::NotFound { .. })
                    if !matches!(ext, PackExt::Pack | PackExt::Idx) =>
                {
                    continue;
                }
                Err(e) => return Err(e.into()),
            }
            blocking(move || pin(&file, mtime)).await?;
        }
        Ok(bytes)
    }

    /// Makes sure MIDX generation `generation` of `repo` is cached with its
    /// bitmap and reverse index, if the bucket holds it, and returns the
    /// bytes downloaded. A generation missing from the bucket only costs
    /// speed: the snapshot then serves its packs without it. The caller
    /// holds [`NodeCache::read`].
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
        create_private_dir_all(&dir).map_err(io_err("create", &dir))?;
        let repo_hex = RepoIdHex::parse(&repo.to_string())?;
        let mut bytes = 0;
        for ext in MIDX_HYDRATION_ORDER {
            let key = Key::Midx {
                repo: repo_hex,
                generation,
                ext,
            };
            match bucket.get_to_file(key, &path(ext)).await {
                Ok(n) => bytes += n,
                Err(StorageError::NotFound { .. }) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(bytes)
    }

    /// Removes whole cached packs and MIDX generations until the cache holds
    /// at most `max_bytes`, counting a file's space as freed only when no
    /// snapshot links it (`st_nlink == 1`), and removing those first, oldest
    /// first (ADR 0004 item 4). A file a snapshot links is removed only when
    /// the unlinked ones aren't enough; its request keeps its link. The
    /// packs and MIDX generation of `keep` (a snapshot just built) are never
    /// removed, so a request never evicts what it hydrated for itself.
    pub(crate) async fn evict(
        &self,
        max_bytes: u64,
        keep: Option<&Layout>,
    ) -> Result<Eviction, GitError> {
        let keep: HashSet<PathBuf> = keep.map_or_else(HashSet::new, |layout| {
            let packs = layout
                .packs
                .iter()
                .map(|p| layout.pack_dir.join(p.as_str()));
            let midx = layout
                .midx
                .iter()
                .map(|(dir, generation)| dir.join(generation.to_string()));
            packs.chain(midx).collect()
        });
        let _exclusive = self.files.write().await;
        let repos = self.repos();
        blocking(move || evict_blocking(&repos, max_bytes, &keep)).await
    }
}

pub(crate) fn midx_file(dir: &Path, generation: u64, ext: MidxExt) -> PathBuf {
    dir.join(format!("{generation}.{}", ext.as_str()))
}

/// Pins `file`'s mtime and makes it read-only: a snapshot's git shares the
/// inode, and must never change a cached file in place.
fn pin(file: &Path, mtime: SystemTime) -> Result<(), GitError> {
    let f = File::open(file).map_err(io_err("open", file))?;
    f.set_times(FileTimes::new().set_modified(mtime))
        .map_err(io_err("set the mtime of", file))?;
    fs::set_permissions(file, fs::Permissions::from_mode(0o444))
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
/// hydrates) and the start-up probe's file.
fn sweep_temp_files(repos: &Path) -> Result<(), GitError> {
    for dir in std::iter::once(repos.to_path_buf()).chain(cache_dirs(repos)?) {
        for entry in fs::read_dir(&dir).map_err(io_err("list", &dir))? {
            let path = entry.map_err(io_err("list", &dir))?.path();
            if is_temp(&path) {
                remove_any(&path)?;
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
    /// reads as not cached.
    files: Vec<(PathBuf, u64)>,
    linked: bool,
    mtime: SystemTime,
}

impl Unit {
    fn bytes(&self) -> u64 {
        self.files.iter().map(|(_, len)| len).sum()
    }
}

fn evict_blocking(
    repos: &Path,
    max_bytes: u64,
    keep: &HashSet<PathBuf>,
) -> Result<Eviction, GitError> {
    let mut units: BTreeMap<PathBuf, Unit> = BTreeMap::new();
    for dir in cache_dirs(repos)? {
        for entry in fs::read_dir(&dir).map_err(io_err("list", &dir))? {
            let path = entry.map_err(io_err("list", &dir))?.path();
            let meta = fs::symlink_metadata(&path).map_err(io_err("stat", &path))?;
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !meta.is_file() || is_temp(&path) {
                continue;
            }
            let (stem, ext) = name.split_once('.').unwrap_or((name, ""));
            let unit = units.entry(dir.join(stem)).or_insert_with(|| Unit {
                files: Vec::new(),
                linked: false,
                mtime: SystemTime::now(),
            });
            unit.linked |= meta.nlink() > 1;
            if let Ok(m) = meta.modified() {
                unit.mtime = unit.mtime.min(m);
            }
            let entry = (path.clone(), meta.len());
            if ext == PackExt::Pack.as_str() || ext == MidxExt::MultiPackIndex.as_str() {
                unit.files.insert(0, entry);
            } else {
                unit.files.push(entry);
            }
        }
    }
    let mut usage: u64 = units.values().map(Unit::bytes).sum();
    let mut units: Vec<Unit> = units
        .into_iter()
        .filter(|(key, _)| !keep.contains(key))
        .map(|(_, unit)| unit)
        .collect();
    units.sort_by_key(|u| (u.linked, u.mtime));
    let mut report = Eviction::default();
    for unit in units {
        if usage <= max_bytes {
            break;
        }
        for (path, len) in &unit.files {
            // Stat again: a snapshot may have ended since the scan. One
            // ending after this stat is only counted as held, never the
            // reverse.
            let linked = match fs::symlink_metadata(path) {
                Ok(meta) => meta.nlink() > 1,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(io_err("stat", path)(e)),
            };
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(io_err("remove", path)(e)),
            }
            if linked {
                report.held_bytes += len;
            } else {
                report.freed_bytes += len;
                usage -= len;
            }
        }
    }
    Ok(report)
}
