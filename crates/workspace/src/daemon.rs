//! The per-user workspace daemon: deduplicates build outputs as they
//! settle, keeps the free-space floor and evicts idle build outputs after
//! the TTL (plan 2026-10-27, Task 6). One runs per state directory.

use std::collections::{HashMap, HashSet};
use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use notify::event::{AccessKind, AccessMode};
use notify::{EventKind, RecursiveMode, Watcher as _};
use tokio::sync::mpsc;

use crate::dedup::TEMP_PREFIX;
use crate::{
    Budget, DedupOutcome, Presets, Registry, WorkspaceError, WorkspaceId, active_workspaces,
    dedup_file, evict_expired, free_to_floor, reconstructible_dirs, volume,
};

/// The daemon's lock file in the state directory, held exclusively while
/// it runs.
pub const DAEMON_LOCK: &str = "daemon.lock";

/// How long a starting daemon retries a held lock: [`daemon_running`]
/// holds it for a moment to probe, and that must not stop a daemon.
const LOCK_WAIT: Duration = Duration::from_secs(2);

/// How often free space is checked against the floor.
const FLOOR_EVERY: Duration = Duration::from_secs(5);

/// How often workspaces past the idle TTL are looked for.
const TTL_EVERY: Duration = Duration::from_mins(10);

/// How long the daemon waits with no workspace registered before exiting;
/// the next `openxet workspace create` starts it again.
const EXIT_WHEN_EMPTY: Duration = Duration::from_mins(10);

/// How often settled files are looked for in the queue.
const SETTLE_POLL: Duration = Duration::from_millis(250);

/// The most files deduplicated per poll, so a large build's queue doesn't
/// keep the daemon from its other checks and from stopping.
const MAX_PER_POLL: usize = 256;

/// The longest between two writes of one workspace's `last_active`.
const TOUCH_EVERY: Duration = Duration::from_secs(30);

/// How long a workspace's reconstructible directories, which take Git to
/// find, are trusted before they are looked up again.
const DIRS_FRESH: Duration = Duration::from_mins(1);

/// The shortest time between two lookups of one workspace's directories
/// when events suggest a new one, such as a first build's `target/`.
const DIRS_MIN: Duration = Duration::from_secs(5);

/// How long events under a directory the daemon evicted are ignored: they
/// are its own deletions, not activity.
const EVICTED_QUIET: Duration = Duration::from_mins(1);

/// Whether a daemon holds the lock in `state_dir`.
#[must_use]
pub fn daemon_running(state_dir: &Path) -> bool {
    let Ok(file) = File::open(state_dir.join(DAEMON_LOCK)) else {
        return false;
    };
    matches!(file.try_lock_shared(), Err(TryLockError::WouldBlock))
}

/// Runs the daemon for the workspaces registered in `state_dir` until
/// `shutdown` completes, the state directory is deleted, or no workspace
/// has been registered for 10 minutes.
///
/// It watches every workspace for filesystem events, which count as
/// activity (`last_active`), and deduplicates each changed file under a
/// reconstructible directory once no event has touched it for
/// [`Budget::settle`]. Only those files: files it already deduplicated and
/// that haven't changed since are skipped, so no clone is cloned again.
/// Every 5 seconds it keeps the floor with [`free_to_floor`], and every
/// 10 minutes it evicts expired workspaces with [`evict_expired`].
///
/// # Errors
///
/// [`WorkspaceError::DaemonRunning`] if another daemon serves `state_dir`,
/// [`WorkspaceError::Io`] if the lock or the filesystem watch can't be set
/// up, and the registry's errors from [`Registry::open`]. Once running, it
/// logs errors and carries on.
pub async fn run_daemon(
    state_dir: &Path,
    budget: Budget,
    shutdown: impl Future<Output = ()>,
) -> Result<(), WorkspaceError> {
    let _lock = lock(state_dir).await?;
    let reg = Registry::open(state_dir).await?;
    let (events, mut received) = mpsc::unbounded_channel();
    // Symbolic links are not followed, so no event comes from outside a
    // workspace; inotify's recursive watch would otherwise walk into them.
    let config = notify::Config::default().with_follow_symlinks(false);
    let watcher = notify::RecommendedWatcher::new(
        move |event| {
            let _ = events.send(event);
        },
        config,
    )
    .map_err(|err| watch_error(state_dir, err))?;
    let mut daemon = Daemon {
        reg,
        budget,
        state_dir: state_dir.to_path_buf(),
        watcher,
        workspaces: HashMap::new(),
        pending: HashMap::new(),
        done: HashMap::new(),
        quiet: Vec::new(),
        unsupported: HashSet::new(),
        empty_since: None,
    };
    daemon.refresh().await;
    tracing::info!(state_dir = %state_dir.display(), "workspace daemon started");

    let mut floor = tokio::time::interval(FLOOR_EVERY);
    let mut ttl = tokio::time::interval(TTL_EVERY);
    let mut settle = tokio::time::interval(SETTLE_POLL);
    for interval in [&mut floor, &mut ttl, &mut settle] {
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    }
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            Some(event) = received.recv() => daemon.on_event(event).await,
            _ = settle.tick() => daemon.dedup_settled().await,
            _ = floor.tick() => {
                if !daemon.keep_floor().await {
                    break;
                }
            }
            _ = ttl.tick() => daemon.expire().await,
        }
    }
    tracing::info!("workspace daemon stopped");
    Ok(())
}

/// A file's identity and last change, to tell a file the daemon already
/// deduplicated from one rewritten since.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Fingerprint {
    dev: u64,
    ino: u64,
    len: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Fingerprint {
    fn of(meta: &std::fs::Metadata) -> Self {
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }

    fn at(path: &Path) -> Option<Self> {
        std::fs::symlink_metadata(path)
            .ok()
            .map(|meta| Self::of(&meta))
    }
}

/// A watched workspace.
struct Watched {
    root: PathBuf,
    presets: Presets,
    /// Its reconstructible directories, as last looked up.
    dirs: Vec<PathBuf>,
    /// When `dirs` was looked up; `None` to look again.
    dirs_at: Option<Instant>,
    last_touch: Option<Instant>,
}

impl Watched {
    fn dirs(&mut self) -> &[PathBuf] {
        if self.dirs_at.is_none_or(|at| at.elapsed() >= DIRS_FRESH) {
            self.dirs = reconstructible_dirs(&self.root, &self.presets);
            self.dirs_at = Some(Instant::now());
        }
        &self.dirs
    }

    /// Whether `path` is under one of the preset directories, which
    /// [`Watched::dirs`] then confirms or not.
    fn may_be_reconstructible(&self, path: &Path) -> bool {
        self.presets
            .dirs()
            .iter()
            .any(|dir| path.starts_with(self.root.join(dir)))
    }
}

struct Daemon {
    reg: Registry,
    budget: Budget,
    state_dir: PathBuf,
    watcher: notify::RecommendedWatcher,
    workspaces: HashMap<WorkspaceId, Watched>,
    /// Changed files waiting to settle, with their last event's time.
    pending: HashMap<PathBuf, Instant>,
    /// Files deduplicated, as they were right after.
    done: HashMap<PathBuf, Fingerprint>,
    /// Directories the daemon evicted, and when.
    quiet: Vec<(PathBuf, Instant)>,
    /// Devices that turned out not to clone files.
    unsupported: HashSet<u64>,
    empty_since: Option<Instant>,
}

impl Daemon {
    /// Watches newly registered workspaces and drops unregistered ones.
    async fn refresh(&mut self) {
        let listed = match self.reg.list(None).await {
            Ok(listed) => listed,
            Err(err) => {
                tracing::warn!(error = %err, "could not read the workspace registry");
                return;
            }
        };
        let ids: HashSet<WorkspaceId> = listed.iter().map(|ws| ws.id).collect();
        let gone: Vec<WorkspaceId> = self
            .workspaces
            .keys()
            .filter(|id| !ids.contains(id))
            .copied()
            .collect();
        for id in gone {
            if let Some(watched) = self.workspaces.remove(&id) {
                let _ = self.watcher.unwatch(&watched.root);
                self.forget_under(&watched.root);
            }
        }
        for ws in listed {
            if self.workspaces.contains_key(&ws.id) {
                continue;
            }
            // Retried on the next refresh: the directory may not exist yet,
            // or be gone with the workspace still registered.
            if let Err(err) = self.watcher.watch(&ws.path, RecursiveMode::Recursive) {
                tracing::debug!(path = %ws.path.display(), error = %err, "not watching workspace");
                continue;
            }
            let presets = Presets::load(&ws.path).unwrap_or_default();
            self.workspaces.insert(
                ws.id,
                Watched {
                    root: ws.path,
                    presets,
                    dirs: Vec::new(),
                    dirs_at: None,
                    last_touch: None,
                },
            );
        }
    }

    async fn on_event(&mut self, event: notify::Result<notify::Event>) {
        let event = match event {
            Ok(event) => event,
            Err(err) => {
                tracing::warn!(error = %err, "filesystem watch error");
                return;
            }
        };
        // Reads, including the daemon's own hashing, are not changes; a
        // file closed after writing is.
        if let EventKind::Access(kind) = event.kind
            && kind != AccessKind::Close(AccessMode::Write)
        {
            return;
        }
        let removed = matches!(event.kind, EventKind::Remove(_));
        self.quiet.retain(|(_, at)| at.elapsed() < EVICTED_QUIET);
        for path in event.paths {
            if path
                .file_name()
                .is_some_and(|name| name.as_encoded_bytes().starts_with(TEMP_PREFIX.as_bytes()))
                || self.quiet.iter().any(|(dir, _)| path.starts_with(dir))
            {
                continue;
            }
            if removed {
                self.pending.remove(&path);
                self.done.remove(&path);
            } else if self
                .done
                .get(&path)
                .is_some_and(|done| Fingerprint::at(&path).as_ref() == Some(done))
            {
                // The daemon's own swap of this file.
                continue;
            }
            let Some((id, watched)) = self
                .workspaces
                .iter_mut()
                .find(|(_, watched)| path.starts_with(&watched.root))
            else {
                continue;
            };
            if watched
                .last_touch
                .is_none_or(|at| at.elapsed() >= TOUCH_EVERY)
            {
                watched.last_touch = Some(Instant::now());
                if let Err(err) = self.reg.touch(id, SystemTime::now()).await {
                    tracing::debug!(error = %err, "could not record activity");
                }
            }
            if removed || !watched.may_be_reconstructible(&path) {
                continue;
            }
            if !watched.dirs.iter().any(|dir| path.starts_with(dir))
                && watched.dirs_at.is_some_and(|at| at.elapsed() >= DIRS_MIN)
            {
                watched.dirs_at = None;
            }
            self.pending.insert(path, Instant::now());
        }
    }

    /// Deduplicates the queued files no event has touched for the settle
    /// window, if they are under a reconstructible directory.
    async fn dedup_settled(&mut self) {
        let settle = self.budget.settle;
        let ready: Vec<PathBuf> = self
            .pending
            .iter()
            .filter(|(_, at)| at.elapsed() >= settle)
            .map(|(path, _)| path.clone())
            .take(MAX_PER_POLL)
            .collect();
        for path in ready {
            self.pending.remove(&path);
            if !self.is_reconstructible(&path) {
                continue;
            }
            let Some(before) = Fingerprint::at(&path) else {
                continue;
            };
            if self.unsupported.contains(&before.dev) || self.done.get(&path) == Some(&before) {
                continue;
            }
            match dedup_file(&self.reg, &path, settle).await {
                Ok(DedupOutcome::Cloned { bytes }) => {
                    if let Err(err) = self.reg.add_deduplicated_bytes(bytes).await {
                        tracing::warn!(error = %err, "could not count deduplicated bytes");
                    }
                    self.record_done(path);
                }
                Ok(DedupOutcome::Canonical | DedupOutcome::TooSmall | DedupOutcome::NotAFile) => {
                    self.record_done(path);
                }
                Ok(DedupOutcome::Unsettled | DedupOutcome::Changed) => {
                    self.pending.insert(path, Instant::now());
                }
                Ok(DedupOutcome::Unsupported) => {
                    tracing::info!(
                        path = %path.display(),
                        "this volume can't clone files, so its files are not deduplicated"
                    );
                    self.unsupported.insert(before.dev);
                }
                // Deleted while queued or hashed, as builds do with
                // temporary files.
                Err(WorkspaceError::Io { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => tracing::warn!(error = %err, "could not deduplicate a file"),
            }
        }
    }

    /// Whether `path` is under a workspace's reconstructible directory,
    /// with no symbolic link on the way: those directories are real ones,
    /// so a path whose parent resolves to itself stays inside them.
    fn is_reconstructible(&mut self, path: &Path) -> bool {
        let Some(watched) = self
            .workspaces
            .values_mut()
            .find(|watched| path.starts_with(&watched.root))
        else {
            return false;
        };
        if !watched.dirs().iter().any(|dir| path.starts_with(dir)) {
            return false;
        }
        path.parent()
            .is_some_and(|parent| parent.canonicalize().is_ok_and(|real| real == parent))
    }

    fn record_done(&mut self, path: PathBuf) {
        if let Some(now) = Fingerprint::at(&path) {
            self.done.insert(path, now);
        }
    }

    /// Checks the floor, after picking up registry changes. Returns whether
    /// to keep running.
    async fn keep_floor(&mut self) -> bool {
        if !self.state_dir.exists() {
            tracing::info!("the state directory is gone");
            return false;
        }
        self.refresh().await;
        if self.workspaces.is_empty() {
            if self.empty_since.get_or_insert_with(Instant::now).elapsed() >= EXIT_WHEN_EMPTY {
                tracing::info!("no workspace registered for 10 minutes");
                return false;
            }
        } else {
            self.empty_since = None;
        }
        match free_to_floor(
            &self.reg,
            &self.budget,
            SystemTime::now(),
            active_workspaces,
            volume,
        )
        .await
        {
            Ok(evicted) => self.evicted(evicted),
            Err(err) => tracing::warn!(error = %err, "could not keep the free-space floor"),
        }
        true
    }

    async fn expire(&mut self) {
        let listed = match self.reg.list(None).await {
            Ok(listed) => listed,
            Err(err) => {
                tracing::warn!(error = %err, "could not read the workspace registry");
                return;
            }
        };
        let Some(active) = active_workspaces(&listed) else {
            tracing::warn!("which workspaces are in use is unknown, so none was expired");
            return;
        };
        match evict_expired(&self.reg, &self.budget, SystemTime::now(), &active).await {
            Ok(evicted) => self.evicted(evicted),
            Err(err) => tracing::warn!(error = %err, "could not evict expired workspaces"),
        }
    }

    fn evicted(&mut self, dirs: Vec<PathBuf>) {
        for dir in dirs {
            self.forget_under(&dir);
            if let Some(watched) = self
                .workspaces
                .values_mut()
                .find(|watched| dir.starts_with(&watched.root))
            {
                watched.dirs_at = None;
            }
            self.quiet.push((dir, Instant::now()));
        }
    }

    fn forget_under(&mut self, dir: &Path) {
        self.pending.retain(|path, _| !path.starts_with(dir));
        self.done.retain(|path, _| !path.starts_with(dir));
    }
}

/// Takes the daemon's lock in `state_dir`, creating both if missing.
async fn lock(state_dir: &Path) -> Result<File, WorkspaceError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state_dir)
        .map_err(WorkspaceError::io(state_dir))?;
    let path = state_dir.join(DAEMON_LOCK);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(WorkspaceError::io(&path))?;
    let deadline = tokio::time::Instant::now() + LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(TryLockError::WouldBlock) => {
                return Err(WorkspaceError::DaemonRunning { lock: path });
            }
            Err(TryLockError::Error(err)) => return Err(WorkspaceError::io(path)(err)),
        }
    }
}

fn watch_error(state_dir: &Path, err: notify::Error) -> WorkspaceError {
    let source = match err.kind {
        notify::ErrorKind::Io(err) => err,
        other => std::io::Error::other(format!("could not watch for file changes: {other:?}")),
    };
    WorkspaceError::Io {
        path: state_dir.to_path_buf(),
        source,
    }
}
