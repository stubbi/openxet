use std::ffi::OsStr;
use std::fmt;
use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sqlx::Row as _;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteRow,
    SqliteSynchronous,
};

use crate::{ProjectId, WorkspaceError};

/// The registry's file name inside the state directory.
const DATABASE: &str = "registry.sqlite3";

/// How long a write waits for another process's write lock before failing.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long an open waits for another process's open to finish.
const OPEN_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// A workspace's identity, a version 7 UUID so ids sort by creation time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WorkspaceId(pub uuid::Uuid);

impl WorkspaceId {
    /// A new id for a workspace being registered now.
    #[must_use]
    pub fn generate() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl fmt::Display for WorkspaceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A registered workspace: one Git worktree of a project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// Its identity.
    pub id: WorkspaceId,
    /// The repository it is a worktree of.
    pub project: ProjectId,
    /// The worktree's root. Callers register and look up canonical paths;
    /// the registry compares them byte for byte.
    pub path: PathBuf,
    /// The branch it was created on; `None` for a detached HEAD.
    pub branch: Option<String>,
    /// When it was registered.
    pub created_at: SystemTime,
    /// When it last saw activity; idle eviction counts from here.
    pub last_active: SystemTime,
    /// Whether it was made outside OpenXet and registered afterwards.
    pub adopted: bool,
}

/// The per-user registry of workspaces and the dedup index of their
/// reconstructible files, in one SQLite file in WAL mode.
///
/// Several processes (the CLI and the daemon) open it at once; writers
/// serialize on SQLite's write lock. Writes go through a pool of one
/// connection so this process's writes queue fairly, and reads through their
/// own pool, which never blocks on the writer in WAL mode
/// (<https://sqlite.org/wal.html#concurrency>).
#[derive(Debug, Clone)]
pub struct Registry {
    writer: SqlitePool,
    reader: SqlitePool,
}

impl Registry {
    /// Opens the registry in `state_dir`, creating the directory (private to
    /// the user) and the database if they are missing, and applies pending
    /// schema migrations.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Io`] if the directory can't be created or locked,
    /// [`WorkspaceError::Database`] if the database can't be opened, and
    /// [`WorkspaceError::Migrate`] if its schema can't be migrated.
    pub async fn open(state_dir: &Path) -> Result<Self, WorkspaceError> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state_dir)
            .map_err(WorkspaceError::io(state_dir))?;
        let path = state_dir.join(DATABASE);
        // Held until the schema is migrated: SQLite answers two first-time
        // opens (the switch to WAL, the migrations' read-then-write
        // transactions) with an immediate `SQLITE_BUSY` that its busy handler
        // never retries, so the CLI and a starting daemon would otherwise
        // race. The same reason as `openxet-index`'s open lock.
        let _lock = open_lock(&path).await?;
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // Every row is rebuildable (see the migration), and the daemon
            // writes the dedup index once per file it hashes, so commits
            // skip the per-commit fsync. WAL with NORMAL never corrupts the
            // database; a power loss can only drop the last commits
            // (https://sqlite.org/pragma.html#pragma_synchronous).
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(BUSY_TIMEOUT);
        let writer = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        sqlx::migrate!()
            .run(&writer)
            .await
            .map_err(WorkspaceError::Migrate)?;
        let reader = SqlitePoolOptions::new().connect_with(options).await?;
        Ok(Self { writer, reader })
    }

    /// Registers `workspace`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Database`] if the write fails, including when its
    /// id or path is already registered.
    pub async fn insert(&self, workspace: &Workspace) -> Result<(), WorkspaceError> {
        sqlx::query(
            "INSERT INTO workspaces (id, project, path, branch, created_at, last_active, adopted) \
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(workspace.id.0.as_bytes().as_slice())
        .bind(workspace.project.as_str())
        .bind(path_bytes(&workspace.path))
        .bind(workspace.branch.as_deref())
        .bind(to_nanos(workspace.created_at)?)
        .bind(to_nanos(workspace.last_active)?)
        .bind(workspace.adopted)
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// The workspace registered at exactly `path`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Database`] if the read fails and
    /// [`WorkspaceError::Corrupt`] if the row doesn't decode.
    pub async fn get_by_path(&self, path: &Path) -> Result<Option<Workspace>, WorkspaceError> {
        sqlx::query(
            "SELECT id, project, path, branch, created_at, last_active, adopted \
             FROM workspaces WHERE path = ?",
        )
        .bind(path_bytes(path))
        .fetch_optional(&self.reader)
        .await?
        .map(|row| workspace_from(&row))
        .transpose()
    }

    /// Every registered workspace, or only `project`'s, oldest first.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Database`] if the read fails and
    /// [`WorkspaceError::Corrupt`] if a row doesn't decode.
    pub async fn list(
        &self,
        project: Option<&ProjectId>,
    ) -> Result<Vec<Workspace>, WorkspaceError> {
        sqlx::query(
            "SELECT id, project, path, branch, created_at, last_active, adopted \
             FROM workspaces WHERE ?1 IS NULL OR project = ?1 ORDER BY id",
        )
        .bind(project.map(ProjectId::as_str))
        .fetch_all(&self.reader)
        .await?
        .iter()
        .map(workspace_from)
        .collect()
    }

    /// Records activity in workspace `id` at `at`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::NotAWorkspace`] if no workspace has that id, and
    /// [`WorkspaceError::Database`] if the write fails.
    pub async fn touch(&self, id: &WorkspaceId, at: SystemTime) -> Result<(), WorkspaceError> {
        let done = sqlx::query("UPDATE workspaces SET last_active = ? WHERE id = ?")
            .bind(to_nanos(at)?)
            .bind(id.0.as_bytes().as_slice())
            .execute(&self.writer)
            .await?;
        if done.rows_affected() == 0 {
            return Err(WorkspaceError::NotAWorkspace(format!("workspace {id}")));
        }
        Ok(())
    }

    /// Unregisters workspace `id`. Its files are untouched.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::NotAWorkspace`] if no workspace has that id, and
    /// [`WorkspaceError::Database`] if the write fails.
    pub async fn remove(&self, id: &WorkspaceId) -> Result<(), WorkspaceError> {
        let done = sqlx::query("DELETE FROM workspaces WHERE id = ?")
            .bind(id.0.as_bytes().as_slice())
            .execute(&self.writer)
            .await?;
        if done.rows_affected() == 0 {
            return Err(WorkspaceError::NotAWorkspace(format!("workspace {id}")));
        }
        Ok(())
    }

    /// The canonical copy of files of `size` bytes whose BLAKE3 hash is
    /// `hash`, as last set. Callers re-check that it still exists and
    /// matches before cloning from it.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Database`] if the read fails.
    pub async fn canonical(
        &self,
        size: u64,
        hash: [u8; 32],
    ) -> Result<Option<PathBuf>, WorkspaceError> {
        let path: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT path FROM canonical_files WHERE size = ? AND hash = ?")
                .bind(to_i64(size, "file size")?)
                .bind(hash.as_slice())
                .fetch_optional(&self.reader)
                .await?;
        Ok(path.map(|bytes| PathBuf::from(OsStr::from_bytes(&bytes))))
    }

    /// Makes `path` the canonical copy of files of `size` bytes whose BLAKE3
    /// hash is `hash`, replacing any earlier one.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Database`] if the write fails.
    pub async fn set_canonical(
        &self,
        size: u64,
        hash: [u8; 32],
        path: &Path,
    ) -> Result<(), WorkspaceError> {
        sqlx::query(
            "INSERT INTO canonical_files (size, hash, path) VALUES (?, ?, ?) \
             ON CONFLICT (size, hash) DO UPDATE SET path = excluded.path",
        )
        .bind(to_i64(size, "file size")?)
        .bind(hash.as_slice())
        .bind(path_bytes(path))
        .execute(&self.writer)
        .await?;
        Ok(())
    }

    /// Drops every canonical copy at `path` or under it, for a directory
    /// about to be evicted or removed. A sibling that only shares a name
    /// prefix (`/ws/one-more` for `/ws/one`) is kept.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Database`] if the write fails.
    pub async fn forget_canonical(&self, path: &Path) -> Result<(), WorkspaceError> {
        let mut root = path_bytes(path);
        while root.len() > 1 && root.ends_with(b"/") {
            root.pop();
        }
        let base = if root == b"/" {
            Vec::new()
        } else {
            root.clone()
        };
        // Byte order, so `[base/, base0)` is exactly the paths under `base`
        // ('0' follows '/') and the range uses the path index.
        let mut start = base.clone();
        start.push(b'/');
        let mut end = base;
        end.push(b'/' + 1);
        sqlx::query("DELETE FROM canonical_files WHERE path = ? OR (path >= ? AND path < ?)")
            .bind(root)
            .bind(start)
            .bind(end)
            .execute(&self.writer)
            .await?;
        Ok(())
    }
}

fn workspace_from(row: &SqliteRow) -> Result<Workspace, WorkspaceError> {
    let id: Vec<u8> = row.try_get("id")?;
    let id = uuid::Uuid::from_slice(&id)
        .map_err(|_| WorkspaceError::Corrupt(format!("a workspace id is {} bytes", id.len())))?;
    let path: Vec<u8> = row.try_get("path")?;
    Ok(Workspace {
        id: WorkspaceId(id),
        project: ProjectId::from_stored(row.try_get("project")?),
        path: PathBuf::from(OsStr::from_bytes(&path)),
        branch: row.try_get("branch")?,
        created_at: from_nanos(row.try_get("created_at")?)?,
        last_active: from_nanos(row.try_get("last_active")?)?,
        adopted: row.try_get("adopted")?,
    })
}

fn path_bytes(path: &Path) -> Vec<u8> {
    path.as_os_str().as_bytes().to_vec()
}

fn to_nanos(at: SystemTime) -> Result<i64, WorkspaceError> {
    let nanos = at
        .duration_since(UNIX_EPOCH)
        .map_err(|_| WorkspaceError::Corrupt("a time before 1970".to_owned()))?
        .as_nanos();
    i64::try_from(nanos).map_err(|_| WorkspaceError::Corrupt("a time after 2262".to_owned()))
}

fn from_nanos(nanos: i64) -> Result<SystemTime, WorkspaceError> {
    let nanos = u64::try_from(nanos)
        .map_err(|_| WorkspaceError::Corrupt(format!("a stored time is {nanos} ns")))?;
    Ok(UNIX_EPOCH + Duration::from_nanos(nanos))
}

fn to_i64(value: u64, what: &str) -> Result<i64, WorkspaceError> {
    i64::try_from(value)
        .map_err(|_| WorkspaceError::Corrupt(format!("{what} {value} is over i64::MAX")))
}

/// Takes an exclusive advisory lock on `{database}-lock`, released when the
/// returned file is dropped.
async fn open_lock(database: &Path) -> Result<File, WorkspaceError> {
    let mut name = database.as_os_str().to_owned();
    name.push("-lock");
    let path = PathBuf::from(name);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&path)
        .map_err(WorkspaceError::io(&path))?;
    let deadline = tokio::time::Instant::now() + OPEN_LOCK_TIMEOUT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(TryLockError::WouldBlock) => {
                return Err(WorkspaceError::Io {
                    path,
                    source: std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "another process held the registry's open lock for 30 s; check that no \
                         OpenXet process is stuck, then retry",
                    ),
                });
            }
            Err(TryLockError::Error(err)) => return Err(WorkspaceError::io(path)(err)),
        }
    }
}
