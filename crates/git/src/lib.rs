//! openxet's Git engine (ADR 0004): Git is served by the `git` binary over
//! refs in the index and packs in the bucket. Each request gets a snapshot
//! repository built from one database snapshot: its refs as `packed-refs`,
//! the pinned config, and hard links to exactly its committed packs in this
//! node's pack cache, hydrated from the bucket on demand. Nodes stay
//! stateless (design §5.3): the cache is a cache.
//!
//! Pushes run `git receive-pack` on such a snapshot; its `proc-receive`
//! hook (openxet itself, [`hook`]) hands the commands back to the engine,
//! which applies the writer reachability rule ([`check_writer_rule`]),
//! publishes the pack and moves the refs in the fenced commit transaction
//! ([`Publisher`], ADR 0004 item 3).
//!
//! The rest of the pack lifecycle (ADR 0004 items 4 and 5) is library code
//! that `openxet gc`, `openxet reindex` and the server's background task
//! ([`run_maintenance`]) call: geometric and full repacks on one
//! leaseholder per repository ([`repack_geometric`], [`repack_full`]), the
//! pack sweep ([`sweep_packs`]), the bucket ref log derived from the
//! database's ([`derive_bucket_ref_log`]), and `reindex` from the bucket
//! with restore and discard ([`reindex_repo`], [`reindex_restore`],
//! [`reindex_discard`]).
//!
//! The Hub API reads and writes with gix in-process, on a request snapshot
//! only: reads by revision ([`Engine::resolve_revision`]) obey R22, and Hub
//! commits, tags and ref writes ([`Engine::hub_commit`],
//! [`Engine::hub_ref_write`]) are published through the same fenced
//! transaction as a push (ADR 0004 item 6).
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
#[cfg(feature = "fault-injection")]
mod fault;
pub mod hook;
mod interpose;
mod lease;
mod ls_refs;
mod maintain;
mod objects;
mod pkt;
mod process;
mod publish;
mod reach;
mod receive;
mod recover;
mod reflog;
mod reindex;
mod repack;
mod request;
mod snapshot;
mod store;
mod sweep;
mod thin;
mod upload;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openxet_index::{HubIndex, IndexError, Oid, Pack, RepoId};
use openxet_storage::{Bucket, PackName, StorageError};

pub use config::{
    Budget, DEFAULT_CACHE_MAX_BYTES, DEFAULT_HUB_COMMIT_WAIT, DEFAULT_MAX_PUSH_BYTES, GitConfig,
};
#[cfg(feature = "fault-injection")]
pub use fault::{FaultHook, FaultPoint};
pub use hook::HookServer;
pub use lease::{DEFAULT_LEASE_TTL, acquire_lease};
pub use maintain::{
    GEOMETRIC_REPACK_PACKS, MAINTENANCE_INTERVAL, MaintenanceReport, maintain, run_maintenance,
};
pub use objects::{
    Blob, CommitInfo, Entry, EntryKind, HubCommit, Ident, LocatedBlob, Op, Pointer, Resolved,
    Revision, Signature, TagInfo, TreeListing, TreePage, canonical_pointer, parse_pointer,
};
#[cfg(feature = "fault-injection")]
pub use objects::{FAULT_OMIT_BLOBS_ACTOR, HubWriteHold};
pub use openxet_index::Lease;
pub use pkt::{
    Fetch, LsRefs, MAX_PKT_LEN, Pkt, PktError, V2Command, err_pkt, find_flush, parse_v0_wants,
    parse_v2_command, read_pkt,
};
pub use process::{MIN_GIT_VERSION, PINNED_CONFIG, git_command, protocol_version};
#[cfg(feature = "fault-injection")]
pub use publish::{FAULT_AFTER_COMMIT_ACTOR, FAULT_AFTER_UPLOAD_ACTOR};
pub use publish::{PackFiles, Publication, PublishError, Publisher};
pub use reach::{Reachability, Refused};
pub use receive::{LARGE_BLOB_REPORT_BYTES, WriterRuleCounts};
pub use recover::{DISCARD_COMMITTED_WARNING, DiscardReport, reindex_discard, reindex_restore};
pub use reflog::{REF_LOG_RECORD_MAX_ENTRIES, derive_bucket_ref_log};
pub use reindex::{REINDEX_ACTOR, ReindexReport, RolledBack, reindex_repo};
#[cfg(feature = "fault-injection")]
pub use repack::{Prepared, prepare_full, prepare_geometric};
pub use repack::{RepackReport, repack_full, repack_geometric};
#[cfg(feature = "fuzzing")]
pub use request::{PackStats, RequestParser};
pub use snapshot::SnapshotRepo;
pub use store::Eviction;
pub use sweep::{SweepReport, SweptPack, sweep_packs};
pub use thin::{
    MISSING_OBJECTS, Proof, SentObjects, THIN_HINT, WalkLimits, WriterRefused, check_writer_rule,
    classify_sent,
};
pub use upload::{ResponseStream, Service};

use process::ProcessEnv;
use snapshot::Layout;
use store::NodeCache;

/// GC's default grace period: 30 days (design §5.6, ruling M1-R5).
pub const DEFAULT_GRACE: Duration = Duration::from_hours(30 * 24);

/// The shortest grace period GC accepts: 28 days, the 21-day client shard
/// cache plus the 7-day dedup key (design §5.6, ruling M1-R5).
pub const MIN_GRACE: Duration = Duration::from_hours(28 * 24);

/// Refuses a grace period below [`MIN_GRACE`].
pub(crate) fn check_grace(grace: Duration) -> Result<(), GitError> {
    if grace < MIN_GRACE {
        return Err(GitError::GraceTooShort { grace });
    }
    Ok(())
}

/// The current time in Unix seconds.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

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
    /// A request was refused before git ran, retryably: the reachability
    /// check is over a concurrency limit (ADR 0004 item 2). Smart HTTP
    /// answers 429; SSH writes [`Refused::pkt`].
    #[error(transparent)]
    Refused(Refused),
    /// The request body couldn't be read.
    #[error("reading the request body failed: {0}; the client may have disconnected, so retry")]
    Body(#[source] std::io::Error),
    /// The snapshot's objects couldn't be read.
    #[error(
        "reading the objects of a Git snapshot failed: {0}; a cached pack may be damaged: stop \
         openxet, empty git.cache_dir and start it again (packs are hydrated from the bucket)"
    )]
    Objects(String),
    /// A grace period below [`MIN_GRACE`].
    #[error(
        "a GC grace period of {grace:?} is too short: it must be at least 28 days (the 21-day \
         client shard cache plus the 7-day dedup key); use the default of 30 days"
    )]
    GraceTooShort {
        /// The grace period asked for.
        grace: Duration,
    },
    /// A repack published nothing: its lease was lost or taken over, its
    /// inputs changed, a full repack's ref log high-water mark was passed,
    /// or its outputs aged out (ADR 0004 item 5). Its outputs are `lost`.
    #[error(
        "the repack published nothing: the repository's refs, packs or repack lease changed while \
         it ran; its outputs are marked lost for GC. Take the lease again and rerun it"
    )]
    RepackFenced,
    /// The repository's repack lease isn't held by the caller, or was lost
    /// or taken over while the pack sweep or `reindex` held it (ADR 0004
    /// items 4 and 5); nothing further was changed.
    #[error(
        "the repository's repack lease is not held, or was lost or taken over while the pack \
         sweep or reindex ran; wait for the holder (a repack or another sweep) to finish, take \
         the lease again and rerun, which finishes what this run started"
    )]
    LeaseLost,
    /// A `reindex restore` was refused.
    #[error("cannot restore {oid} as {refname}: {reason}")]
    Restore {
        /// The object asked for.
        oid: Oid,
        /// The ref asked for.
        refname: String,
        /// Why, and what to do.
        reason: String,
    },
    /// A Hub read's revision is no ref of the snapshot, an object id R22
    /// refuses or the snapshot lacks, or a tree or blob id; or a Hub ref
    /// write's target is. One answer for all of them, so it doesn't tell
    /// "exists but unreachable" from "absent" (ADR 0004 item 6).
    #[error(
        "revision {revision} not found; name a branch, a tag, or a commit that one of them \
         contains"
    )]
    RevisionNotFound {
        /// The revision as the caller named it.
        revision: String,
    },
    /// A Hub read's path names no entry of the kind asked for.
    #[error("{path:?} was not found at this revision; list the revision's tree to see its paths")]
    EntryNotFound {
        /// The path as the caller named it.
        path: String,
    },
    /// A Hub commit's `parentCommit` isn't its branch's tip (ruling M2-R7).
    #[error(
        "the parent commit {expected} is not the tip of {branch} (which is {}); read the \
         branch's current commit and commit on it",
        .actual.map_or_else(|| "missing".to_owned(), |o| o.to_string())
    )]
    ParentMismatch {
        /// The branch, as a full ref name.
        branch: String,
        /// The parent the client sent.
        expected: Oid,
        /// The branch's tip in the snapshot.
        actual: Option<Oid>,
    },
    /// A Hub ref write's ref isn't at the target the caller expected: it
    /// exists already, or doesn't, or another writer moved it.
    #[error(
        "{name} is not at the expected target (it exists already, is missing, or moved); read the refs again and retry"
    )]
    RefConflict {
        /// The full ref name.
        name: String,
    },
    /// A Hub write can't be applied as asked: the message says why.
    #[error("{0}")]
    InvalidHubWrite(String),
    /// A Hub write would make reachable an object it didn't send and no ref
    /// reaches (the writer reachability rule, ADR 0004 item 3), or its
    /// objects aren't connected in its snapshot. The text is git's for a
    /// missing object.
    #[error(
        "{MISSING_OBJECTS}: the write names an object it doesn't carry and no branch or tag \
         reaches"
    )]
    MissingObjects,
    /// R22's concurrency limits are full for this repository or writer
    /// (ADR 0004 item 2), or a Hub commit waited longer than
    /// `hub.commit_wait` for its turn on its branch; retryable.
    #[error(
        "too many reads or writes are running in this repository right now; retry in a few \
         seconds"
    )]
    Busy,
    /// A Hub write lost its commit transaction on every attempt to
    /// concurrent writers or full repacks; retryable.
    #[error("{name} kept moving while this write ran; retry it")]
    Contended {
        /// The full ref name.
        name: String,
    },
    /// A fault-injection point stopped a Hub write, as a crash would.
    #[cfg(feature = "fault-injection")]
    #[error("fault injected: {0}")]
    Injected(&'static str),
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
    empty_receive_advertisement: Vec<u8>,
    reach: Reachability,
    counters: Arc<receive::Counters>,
    /// Per repository, the highest `ref_log` row this engine knows the
    /// bucket ref log holds ([`derive_bucket_ref_log`]), each behind its own
    /// lock so one repository's derivation never waits on another's.
    derived: std::sync::Mutex<HashMap<RepoId, Arc<tokio::sync::Mutex<Option<u64>>>>>,
    #[cfg(feature = "fault-injection")]
    hooks: fault::Hooks,
    hub: objects::HubWrites,
    /// The helper threads `tree` pages read blobs on, bounded process-wide.
    page_helpers: Arc<objects::HelperPermits>,
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
    /// finds `git` on `PATH` and checks it is 2.37 or newer (item 8),
    /// installs the `proc-receive` hook (item 3: a symlink to
    /// `config.hook_program`, by default the running executable), and
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
        let bootstrap = ProcessEnv::bootstrap(&git, &cache.home())?;
        process::check_version(&git, &bootstrap).await?;
        let exec_dir = process::exec_path(&git, &bootstrap).await?;
        let process = Arc::new(ProcessEnv::new(
            &git,
            &exec_dir,
            &cache.hooks(),
            &cache.home(),
        )?);
        install_hook(&cache.hooks(), config.hook_program.as_deref())?;
        let reach = Reachability::new(config.reachability);
        let mut engine = Self {
            index,
            bucket,
            config,
            git,
            cache,
            process,
            v2_advertisement: Vec::new(),
            empty_receive_advertisement: Vec::new(),
            reach,
            counters: Arc::default(),
            derived: std::sync::Mutex::default(),
            #[cfg(feature = "fault-injection")]
            hooks: fault::Hooks::default(),
            hub: objects::HubWrites::default(),
            page_helpers: Arc::new(objects::HelperPermits::per_cpu()),
        };
        engine.v2_advertisement = engine.capture_advertisement(Service::UploadPack, 2).await?;
        engine.empty_receive_advertisement = ls_refs::with_openxet_agent(
            &engine
                .capture_advertisement(Service::ReceivePack, 0)
                .await?,
        );
        Ok(engine)
    }

    /// The git this engine runs, for [`git_command`].
    #[must_use]
    pub fn git(&self) -> &Path {
        &self.git
    }

    /// `git.max_push_bytes`: the largest pack one push may send.
    #[must_use]
    pub fn max_push_bytes(&self) -> u64 {
        self.config.max_push_bytes
    }

    /// The engine's R22 concurrency limits, which every walk shares: R22's
    /// own and the writer rule's thin-base walk ([`WalkLimits`], ADR 0004
    /// items 2 and 3).
    #[must_use]
    pub fn reachability(&self) -> &Reachability {
        &self.reach
    }

    /// git's protocol v2 capability advertisement, as `git upload-pack`
    /// printed it at start-up with the pinned config, pkt-lines and the
    /// closing flush included. Not yet filtered to ADR 0004 item 2's
    /// allowlist.
    #[must_use]
    pub fn v2_advertisement(&self) -> &[u8] {
        &self.v2_advertisement
    }

    /// The process-wide limit on `tree` page helper threads, and the most
    /// ever in use at once. Test-only.
    #[cfg(feature = "fault-injection")]
    #[must_use]
    pub fn page_helpers(&self) -> (usize, usize) {
        (self.page_helpers.limit(), self.page_helpers.peak())
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
        self.snapshot_of(repo, snap.refs, snap.packs, snap.full_repack_gen, true)
            .await
    }

    /// Builds a snapshot repository of `repo` holding `refs` and exactly
    /// `packs`, hydrated like [`Engine::snapshot`]'s, with the newest MIDX
    /// generation among them when `with_midx`. The caller chooses the set:
    /// requests take the committed packs of one database snapshot; a repack
    /// takes those, `reindex` every pack in the bucket.
    pub(crate) async fn snapshot_of(
        &self,
        repo: RepoId,
        refs: Vec<(String, Oid)>,
        packs: Vec<Pack>,
        full_repack_gen: u64,
        with_midx: bool,
    ) -> Result<SnapshotRepo, GitError> {
        let names = packs
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
        for (name, _) in &refs {
            snapshot::check_ref_name(name)?;
        }
        let mut refs = refs;
        refs.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        let midx_gen = packs
            .iter()
            .filter_map(|p| p.midx_gen)
            .max()
            .filter(|_| with_midx);

        // Download without the lock, then check again under it: eviction
        // may have removed a pack in between, and the second pass hydrates
        // it again (a stat per pack otherwise).
        let mut downloaded = self.hydrate(repo, &packs, &names, midx_gen).await?;
        let shared = self.cache.read().await;
        downloaded += self.hydrate(repo, &packs, &names, midx_gen).await?;
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
            repo,
            dir,
            refs,
            packs,
            full_repack_gen,
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

    /// The `receive-pack` advertisement of a repository without refs, as
    /// [`Engine::advertise`] would print it for one: what a push to a
    /// repository that doesn't exist yet is answered, so that its first
    /// push creates it (ruling M2-R11). Captured from git at start-up, so
    /// its capabilities are git's own, with openxet's `agent`.
    #[must_use]
    pub fn empty_receive_advertisement(&self) -> &[u8] {
        &self.empty_receive_advertisement
    }

    /// `git {service} --advertise-refs` over `protocol` on an empty
    /// repository of no repository.
    async fn capture_advertisement(
        &self,
        service: Service,
        protocol: u8,
    ) -> Result<Vec<u8>, GitError> {
        let layout = Layout {
            snapshots: self.cache.snapshots(),
            hooks: self.cache.hooks(),
            refs: Vec::new(),
            pack_dir: self.cache.snapshots(),
            packs: Vec::new(),
            midx: None,
        };
        // An empty repository of no repository: the all-zero id is never
        // generated (ids are UUIDv7) and never checked for reachability.
        let empty = SnapshotRepo {
            repo: RepoId([0; 16]),
            dir: blocking(move || snapshot::build(&layout)).await?,
            refs: Vec::new(),
            packs: Vec::new(),
            full_repack_gen: 0,
            process: Arc::clone(&self.process),
        };
        let args = [service.as_str(), "--advertise-refs", "--stateless-rpc", "."];
        let out = git_command(&self.git, &empty, Some(protocol))
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

/// Links `{hooks}/proc-receive` to `program`, or to the running executable:
/// the hook is openxet itself, which runs `openxet proc-receive` when
/// started under that name (ADR 0004 item 3).
fn install_hook(hooks: &Path, program: Option<&Path>) -> Result<(), GitError> {
    let program = match program {
        Some(p) => p.to_path_buf(),
        None => {
            std::env::current_exe().map_err(io_err("find the running executable for", hooks))?
        }
    };
    let link = hooks.join("proc-receive");
    std::os::unix::fs::symlink(&program, &link)
        .map_err(io_err("install the proc-receive hook", &link))
}
