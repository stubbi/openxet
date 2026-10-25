//! openxet's Git engine (ADR 0004): Git is served by the `git` binary over
//! refs in the index and packs in the bucket. Each request gets a snapshot
//! repository built from one database snapshot: its refs as `packed-refs`,
//! the pinned config, and hard links to exactly its committed packs in this
//! node's pack cache, hydrated from the bucket on demand. Nodes stay
//! stateless (design §5.3): the cache is a cache.
//!
//! git runs as a separate program with a sanitised environment (ADR 0004
//! items 7 and 8); `git` 2.37 or newer must be on `PATH`. Unix only, like
//! the hard links and file descriptors it relies on.
//!
//! A [`SnapshotRepo`] deletes its directory when dropped, so it must outlive
//! every git child running in it: keep it alive until the child has exited
//! and its output is read.

#![forbid(unsafe_code)]

mod config;
mod process;
mod snapshot;
mod store;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use openxet_index::{HubIndex, IndexError, Pack, RepoId};
use openxet_storage::{Bucket, PackName, StorageError};

pub use config::{Budget, DEFAULT_CACHE_MAX_BYTES, GitConfig};
pub use process::{MIN_GIT_VERSION, PINNED_CONFIG, git_command, protocol_version};
pub use snapshot::SnapshotRepo;
pub use store::Eviction;

use process::ProcessEnv;
use snapshot::Layout;
use store::NodeCache;

/// A Git engine failure. Messages say what happened and what to do next;
/// they carry paths, ids and git's own messages, never credentials.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitError {
    /// No `git` executable is on `PATH`.
    #[error(
        "no git executable found on PATH; the Git role needs git 2.37 or newer: install it (for \
         example `apt install git`) and start openxet again"
    )]
    GitNotFound,
    /// The `git` on `PATH` is older than [`MIN_GIT_VERSION`], or its version
    /// can't be read.
    #[error(
        "the Git role needs git 2.37 or newer (for proc-receive, multi-pack-index bitmaps and \
         cruft packs), but {} reports {found:?}; install a newer git and put it first on PATH",
        .path.display()
    )]
    GitVersion {
        /// The git that was found.
        path: PathBuf,
        /// What `git --version` printed.
        found: String,
    },
    /// git failed where it should not.
    #[error(
        "`git {command}` failed ({status}): {stderr}; check that {} is a working git 2.37 or newer",
        .git.display()
    )]
    GitFailed {
        /// The git that ran.
        git: PathBuf,
        /// Its arguments.
        command: String,
        /// How it exited.
        status: String,
        /// What it printed on stderr.
        stderr: String,
    },
    /// Snapshots can't hard-link cached packs (ADR 0004 item 4).
    #[error(
        "cannot hard-link from the Git pack cache {} into the snapshot directory {}: {reason}; \
         snapshots link cached packs, so both must be on one filesystem that makes hard links \
         (ext4, XFS, Btrfs, ZFS or APFS, not FAT or most network mounts): point git.cache_dir at \
         such a filesystem and don't mount its snapshots directory separately",
        .cache.display(),
        .snapshots.display()
    )]
    CacheFilesystem {
        /// The pack cache.
        cache: PathBuf,
        /// The snapshot directory.
        snapshots: PathBuf,
        /// What the check found.
        reason: String,
    },
    /// The cache path isn't UTF-8; it is written into every snapshot's
    /// config and git's `PATH`.
    #[error(
        "git.cache_dir {} is not valid UTF-8; choose a path made of UTF-8 characters",
        .path.display()
    )]
    CacheDirNotUtf8 {
        /// The path.
        path: PathBuf,
    },
    /// Another process holds the cache.
    #[error(
        "the Git cache {} is in use by another openxet process; give each process its own \
         git.cache_dir",
        .path.display()
    )]
    CacheInUse {
        /// The cache directory.
        path: PathBuf,
    },
    /// A filesystem operation on the cache failed.
    #[error(
        "failed to {op} {}: {source}; check that git.cache_dir is writable and has free space",
        .path.display()
    )]
    Io {
        /// What openxet was doing.
        op: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The index failed.
    #[error(transparent)]
    Index(#[from] IndexError),
    /// The bucket failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The index holds a Git record openxet would never write.
    #[error(
        "the index holds an invalid Git record ({0}); the bucket is unaffected. Please report \
         this at https://github.com/stubbi/openxet/issues"
    )]
    Corrupt(String),
}

pub(crate) fn io_err<'a>(
    op: &'static str,
    path: &'a Path,
) -> impl FnOnce(std::io::Error) -> GitError + 'a {
    move |source| GitError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

/// Runs filesystem work off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, GitError> + Send + 'static,
) -> Result<T, GitError> {
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// The Git engine of one process: the index, the bucket, the node cache and
/// the git it runs. The server holds it in an `Arc`.
pub struct Engine {
    index: Arc<dyn HubIndex>,
    bucket: Arc<dyn Bucket>,
    config: GitConfig,
    git: PathBuf,
    cache: NodeCache,
    process: Arc<ProcessEnv>,
    v2_advertisement: Vec<u8>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("config", &self.config)
            .field("git", &self.git)
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Starts the engine: opens the node cache at `config.cache_dir` (one
    /// process at a time), sweeps snapshots a previous process leaked,
    /// checks that snapshots can hard-link cached packs (ADR 0004 item 4),
    /// finds `git` on `PATH` and checks it is 2.37 or newer (item 8), and
    /// captures git's protocol v2 capability advertisement (item 2).
    ///
    /// # Errors
    ///
    /// [`GitError::GitNotFound`], [`GitError::GitVersion`],
    /// [`GitError::CacheFilesystem`], [`GitError::CacheInUse`],
    /// [`GitError::GitFailed`] and [`GitError::Io`].
    pub async fn start(
        index: Arc<dyn HubIndex>,
        bucket: Arc<dyn Bucket>,
        config: GitConfig,
    ) -> Result<Self, GitError> {
        let git = process::find_git()?;
        let dir = config.cache_dir.clone();
        let cache = blocking(move || NodeCache::open(&dir)).await?;
        let bootstrap = ProcessEnv::bootstrap(&cache.home())?;
        process::check_version(&git, &bootstrap).await?;
        let exec_dir = process::exec_path(&git, &bootstrap).await?;
        let process = Arc::new(ProcessEnv::new(&exec_dir, &cache.hooks(), &cache.home())?);
        let mut engine = Self {
            index,
            bucket,
            config,
            git,
            cache,
            process,
            v2_advertisement: Vec::new(),
        };
        engine.v2_advertisement = engine.capture_v2_advertisement().await?;
        Ok(engine)
    }

    /// The git this engine runs, for [`git_command`].
    #[must_use]
    pub fn git(&self) -> &Path {
        &self.git
    }

    /// git's protocol v2 capability advertisement, as `git upload-pack`
    /// printed it at start-up with the pinned config, pkt-lines and the
    /// closing flush included. Not yet filtered to ADR 0004 item 2's
    /// allowlist.
    #[must_use]
    pub fn v2_advertisement(&self) -> &[u8] {
        &self.v2_advertisement
    }

    /// Builds a snapshot repository of `repo` from one database snapshot
    /// (ADR 0004 items 1 and 4): hydrates the committed packs this node lacks
    /// (and the current MIDX generation), then writes `packed-refs` and the
    /// pinned config and hard-links exactly the committed packs. Evicts
    /// when hydration took the cache over `cache_max_bytes`.
    ///
    /// # Errors
    ///
    /// [`GitError::Index`] (an unknown repository included),
    /// [`GitError::Storage`] if a pack can't be hydrated,
    /// [`GitError::Corrupt`] for a pack or ref name openxet would never
    /// write, and [`GitError::Io`].
    pub async fn snapshot(&self, repo: RepoId) -> Result<SnapshotRepo, GitError> {
        let snap = self.index.snapshot(repo).await?;
        let names = snap
            .packs
            .iter()
            .map(|p| {
                PackName::parse(&p.name).map_err(|_| {
                    GitError::Corrupt(format!(
                        "pack {} of repository {repo} is named {:?}",
                        p.id,
                        p.name.escape_debug().to_string()
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (name, _) in &snap.refs {
            snapshot::check_ref_name(name)?;
        }
        let mut refs = snap.refs;
        refs.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let midx_gen = snap.packs.iter().filter_map(|p| p.midx_gen).max();

        // Download without the lock, then check again under it: eviction
        // may have removed a pack in between, and the second pass hydrates
        // it again (a stat per pack otherwise).
        let mut downloaded = self.hydrate(repo, &snap.packs, &names, midx_gen).await?;
        let shared = self.cache.read().await;
        downloaded += self.hydrate(repo, &snap.packs, &names, midx_gen).await?;
        let layout = Layout {
            snapshots: self.cache.snapshots(),
            hooks: self.cache.hooks(),
            refs: refs.clone(),
            pack_dir: self.cache.repo_dir(repo),
            packs: names,
            midx: midx_gen.map(|g| (self.cache.midx_dir(repo), g)),
        };
        let dir = blocking(move || snapshot::build(&layout)).await?;
        drop(shared);

        // The snapshot is good whatever eviction does; its own packs are
        // linked now, so eviction can't take them.
        if downloaded > 0 {
            match self.evict(self.config.cache_max_bytes).await {
                Ok(e) if e.deficit_bytes > 0 => tracing::warn!(
                    deficit_bytes = e.deficit_bytes,
                    "the Git cache is over git.cache_max_bytes with packs that requests in flight \
                     hold; it shrinks as they finish. Raise git.cache_max_bytes if this persists"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "evicting from the Git cache failed"),
            }
        }
        Ok(SnapshotRepo {
            dir,
            refs,
            packs: snap.packs,
            full_repack_gen: snap.full_repack_gen,
            process: Arc::clone(&self.process),
        })
    }

    /// Evicts cached packs no snapshot links until the cache holds at most
    /// `max_bytes` (ADR 0004 item 4), and reports what linked packs keep it
    /// over. [`Engine::snapshot`] does this with `cache_max_bytes` after
    /// hydrating.
    ///
    /// # Errors
    ///
    /// [`GitError::Io`].
    pub async fn evict(&self, max_bytes: u64) -> Result<Eviction, GitError> {
        self.cache.evict(max_bytes).await
    }

    /// Hydrates `packs` (named `names`) and MIDX generation `midx_gen` of
    /// `repo`, returning the bytes downloaded.
    async fn hydrate(
        &self,
        repo: RepoId,
        packs: &[Pack],
        names: &[PackName],
        midx_gen: Option<u64>,
    ) -> Result<u64, GitError> {
        let mut downloaded = 0;
        for (pack, name) in packs.iter().zip(names) {
            downloaded += self
                .cache
                .hydrate_pack(self.bucket.as_ref(), repo, name, pack.created_at)
                .await?;
        }
        if let Some(generation) = midx_gen {
            downloaded += self
                .cache
                .hydrate_midx(self.bucket.as_ref(), repo, generation)
                .await?;
        }
        Ok(downloaded)
    }

    async fn capture_v2_advertisement(&self) -> Result<Vec<u8>, GitError> {
        let layout = Layout {
            snapshots: self.cache.snapshots(),
            hooks: self.cache.hooks(),
            refs: Vec::new(),
            pack_dir: self.cache.snapshots(),
            packs: Vec::new(),
            midx: None,
        };
        let empty = SnapshotRepo {
            dir: blocking(move || snapshot::build(&layout)).await?,
            refs: Vec::new(),
            packs: Vec::new(),
            full_repack_gen: 0,
            process: Arc::clone(&self.process),
        };
        let args = ["upload-pack", "--advertise-refs", "--stateless-rpc", "."];
        let out = git_command(&self.git, &empty, Some(2))
            .args(args)
            .output()
            .await
            .map_err(io_err("run", &self.git))?;
        if !out.status.success() {
            return Err(GitError::GitFailed {
                git: self.git.clone(),
                command: args.join(" "),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(out.stdout)
    }
}
