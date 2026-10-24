//! Local-disk bucket.
//!
//! On-disk layout under the root (what `openxet reindex` walks, issue #26):
//!
//! ```text
//! {root}/xorbs/default/{hex[0..2]}/{hex}                          Key::Xorb
//! {root}/shards/{hex[0..2]}/{hex}                                 Key::Shard
//! {root}/registrations/{scope}/{tenant}/{hex[0..2]}/{hex}         Key::Registration
//! {root}/xorb-owners/{tenant}/{hex[0..2]}/{hex}                   Key::XorbOwner
//! {root}/.tmp/{uuid}                                              in-flight writes
//! ```
//!
//! `hex` is [`hash_to_hex`] of the key's xorb or shard; `{scope}` and
//! `{tenant}` are [`NameHash`]es, 64 hex digits, so no name ever becomes a
//! path of its own. The two-character prefix directory keeps any one
//! directory from growing unbounded. `.tmp` lives under the root so the final
//! rename (or, for [`Bucket::create`], hard link) never crosses a filesystem
//! and stays atomic.
//!
//! On unix, every directory the bucket creates is mode `0700` and every
//! object `0600`: the bucket holds every scope's data, and only the server
//! may read it. The umask can only narrow these modes, never widen them.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use openxet_protocol::{MerkleHash, hash_from_hex, hash_to_hex};

use crate::{
    Bucket, Key, KeyKind, NameHash, RangeStream, STREAM_BLOCK_BYTES, StorageError,
    create_private_dir_all,
};

const TMP_DIR: &str = ".tmp";
const XORBS_DIR: &str = "xorbs/default";
const SHARDS_DIR: &str = "shards";
const REGISTRATIONS_DIR: &str = "registrations";
const XORB_OWNERS_DIR: &str = "xorb-owners";
/// Temp files older than this are leftovers from a crash; no healthy write
/// stays in flight for an hour.
const STALE_TEMP_AGE: Duration = Duration::from_hours(1);

/// The filesystem calls that make a write durable, behind a seam so tests can
/// observe that each one happens and in which order. Crate-private.
pub(crate) trait Durability: Send + Sync + std::fmt::Debug + 'static {
    fn sync_file(&self, file: &File) -> std::io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn hard_link(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()>;
}

#[derive(Debug)]
struct RealFs;

impl Durability for RealFs {
    fn sync_file(&self, file: &File) -> std::io::Result<()> {
        file.sync_all()
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        fs::rename(from, to)
    }
    fn hard_link(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        fs::hard_link(from, to)
    }
    fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
        File::open(dir)?.sync_all()
    }
}

/// A [`Bucket`] on a local filesystem. Share it behind an `Arc`.
#[derive(Debug)]
pub struct LocalBucket {
    root: PathBuf,
    fs: Arc<dyn Durability>,
}

impl LocalBucket {
    /// Opens (creating if needed) the bucket at `root`, deletes temp files
    /// older than one hour left behind by crashed writes, and checks that the
    /// filesystem makes hard links, which [`Bucket::create`] needs.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the directories cannot be created or listed,
    /// and [`StorageError::NoHardLinks`] if the filesystem lacks hard links.
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        Self::open_with(root, Arc::new(RealFs))
    }

    pub(crate) fn open_with(root: &Path, fs: Arc<dyn Durability>) -> Result<Self, StorageError> {
        let tmp = root.join(TMP_DIR);
        let xorbs = root.join("xorbs");
        let skeleton = [
            tmp.clone(),
            root.join(SHARDS_DIR),
            root.join(XORBS_DIR),
            root.join(REGISTRATIONS_DIR),
            root.join(XORB_OWNERS_DIR),
        ];
        for dir in &skeleton {
            create_private_dir_all(dir).map_err(io("create bucket directory", dir))?;
        }
        // Without this, a power loss right after the first put can drop the
        // skeleton's own directory entries and the object with them.
        for dir in skeleton
            .iter()
            .map(PathBuf::as_path)
            .chain([xorbs.as_path(), root])
        {
            fsync_dir(fs.as_ref(), dir)?;
        }
        sweep_stale_temp(&tmp)?;
        probe_hard_links(fs.as_ref(), root, &tmp)?;
        Ok(Self {
            root: root.to_path_buf(),
            fs,
        })
    }

    fn place(&self, key: &Key) -> Place {
        let (base, dir, hash) = match key {
            Key::Xorb(h) => {
                let base = self.root.join(XORBS_DIR);
                (base.clone(), base, h)
            }
            Key::Shard(h) => {
                let base = self.root.join(SHARDS_DIR);
                (base.clone(), base, h)
            }
            Key::Registration {
                scope,
                tenant,
                shard,
            } => {
                let base = self.root.join(REGISTRATIONS_DIR);
                let dir = base.join(scope.to_string()).join(tenant.to_string());
                (base, dir, shard)
            }
            Key::XorbOwner { tenant, xorb } => {
                let base = self.root.join(XORB_OWNERS_DIR);
                let dir = base.join(tenant.to_string());
                (base, dir, xorb)
            }
        };
        let hex = hash_to_hex(hash);
        Place {
            base,
            path: dir.join(&hex[..2]).join(hex),
        }
    }

    fn path(&self, key: &Key) -> PathBuf {
        self.place(key).path
    }
}

/// Checks that the filesystem under `root` makes hard links, which
/// [`Bucket::create`] publishes records with, so a bucket that couldn't
/// write them fails to open rather than failing every shard upload.
fn probe_hard_links(fs: &dyn Durability, root: &Path, tmp: &Path) -> Result<(), StorageError> {
    let probe = tmp.join(format!("{}.link-probe", uuid::Uuid::new_v4().simple()));
    let link = probe.with_extension("link-probe-link");
    create_private_file(&probe).map_err(io("create temp file", &probe))?;
    let linked = fs.hard_link(&probe, &link);
    // Best effort: a leftover is swept by the next `open`.
    let _ = fs::remove_file(&link);
    let _ = fs::remove_file(&probe);
    linked.map_err(|source| StorageError::NoHardLinks {
        path: root.to_path_buf(),
        source,
    })
}

/// Creates the new file `path`, mode `0600` on unix.
fn create_private_file(path: &Path) -> std::io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn io<'a>(op: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> StorageError + 'a {
    move |source| StorageError::Io {
        op,
        path: path.to_path_buf(),
        source,
    }
}

fn sweep_stale_temp(tmp: &Path) -> Result<(), StorageError> {
    let now = SystemTime::now();
    for entry in fs::read_dir(tmp).map_err(io("list temp directory", tmp))? {
        let entry = entry.map_err(io("list temp directory", tmp))?;
        let Ok(modified) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if now
            .duration_since(modified)
            .is_ok_and(|age| age > STALE_TEMP_AGE)
        {
            // Another node may be sweeping too; a vanished file is fine.
            match fs::remove_file(entry.path()) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io("remove stale temp file", &entry.path())(e)),
            }
        }
    }
    Ok(())
}

fn fsync_dir(durable: &dyn Durability, dir: &Path) -> Result<(), StorageError> {
    durable.sync_dir(dir).map_err(io("fsync directory", dir))
}

fn existing_size(path: &Path) -> Result<Option<u64>, StorageError> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some(m.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io("stat", path)(e)),
    }
}

/// Where an object lives: its kind's skeleton directory, which `open` made
/// durable, and its path somewhere below it.
#[derive(Debug, Clone)]
struct Place {
    base: PathBuf,
    path: PathBuf,
}

impl Place {
    /// The directory holding the object.
    fn dir(&self) -> &Path {
        self.path.parent().unwrap_or(&self.path)
    }

    /// The directories from `base` down to, not including, [`Place::dir`]:
    /// each holds the entry of the next, so each is fsynced once the next
    /// exists.
    fn ancestors(&self) -> Vec<&Path> {
        let mut dirs: Vec<&Path> = self
            .dir()
            .ancestors()
            .skip(1)
            .take_while(|d| d.starts_with(&self.base))
            .collect();
        dirs.reverse();
        dirs
    }
}

/// Makes every directory entry leading to the object durable, from its
/// kind's skeleton directory down, then the object's own entry. Whichever
/// write created them, no write returns `Ok` before they are on disk.
fn sync_object_dirs(durable: &dyn Durability, place: &Place) -> Result<(), StorageError> {
    for dir in place.ancestors() {
        fsync_dir(durable, dir)?;
    }
    fsync_dir(durable, place.dir())
}

/// How a written temp file becomes the object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publish {
    /// [`Bucket::put`]: rename over the path. Content-addressed keys mean a
    /// concurrent writer renames the same bytes.
    Rename,
    /// [`Bucket::create`]: hard-link to the path, which fails if it exists,
    /// so the first writer's bytes stay.
    Link,
}

fn put_blocking(
    durable: &dyn Durability,
    root: &Path,
    key: Key,
    place: &Place,
    bytes: &[u8],
) -> Result<(), StorageError> {
    let new = bytes.len() as u64;
    if let Some(existing) = existing_size(&place.path)? {
        if existing != new {
            return Err(StorageError::SizeConflict {
                key: Box::new(key),
                existing,
                new,
            });
        }
        // The object may come from a concurrent put that has not finished its
        // fsyncs; our Ok must not outrun them.
        return sync_object_dirs(durable, place);
    }
    publish_new(durable, root, place, bytes, Publish::Rename).map(drop)
}

fn create_blocking(
    durable: &dyn Durability,
    root: &Path,
    place: &Place,
    bytes: &[u8],
) -> Result<bool, StorageError> {
    if existing_size(&place.path)?.is_some() {
        // As in `put_blocking`: the creator may not have finished its fsyncs.
        sync_object_dirs(durable, place)?;
        return Ok(false);
    }
    publish_new(durable, root, place, bytes, Publish::Link)
}

/// Writes `bytes` to a temp file and publishes it at `place`. Returns
/// whether this call's bytes are the object's: always for a rename, and
/// for a link unless another writer's object was there first.
fn publish_new(
    durable: &dyn Durability,
    root: &Path,
    place: &Place,
    bytes: &[u8],
    how: Publish,
) -> Result<bool, StorageError> {
    let tmp_path = root
        .join(TMP_DIR)
        .join(uuid::Uuid::new_v4().simple().to_string());
    let result = write_and_publish(durable, &tmp_path, place, bytes, how);
    // A renamed temp file is gone already. After a link, or a failure, the
    // temp name is removed on a best-effort basis: a leftover is swept by the
    // next `open`, and the object never depends on it.
    if how == Publish::Link || result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn write_and_publish(
    durable: &dyn Durability,
    tmp_path: &Path,
    place: &Place,
    bytes: &[u8],
    how: Publish,
) -> Result<bool, StorageError> {
    let mut file = create_private_file(tmp_path).map_err(io("create temp file", tmp_path))?;
    file.write_all(bytes)
        .map_err(io("write temp file", tmp_path))?;
    durable
        .sync_file(&file)
        .map_err(io("fsync temp file", tmp_path))?;
    drop(file);

    let dir = place.dir();
    create_private_dir_all(dir).map_err(io("create directory", dir))?;
    // Before publishing, so a crash never leaves the object under a
    // directory whose own entry was lost.
    for ancestor in place.ancestors() {
        fsync_dir(durable, ancestor)?;
    }
    let final_path = &place.path;
    let created = match how {
        Publish::Rename => {
            durable
                .rename(tmp_path, final_path)
                .map_err(io("rename into place", final_path))?;
            true
        }
        Publish::Link => match durable.hard_link(tmp_path, final_path) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(io("link into place", final_path)(e)),
        },
    };
    fsync_dir(durable, dir)?;
    Ok(created)
}

/// The keys of `kind` stored under `root`.
fn list_blocking(root: &Path, kind: KeyKind) -> Result<Vec<Key>, StorageError> {
    let (base, depth) = match kind {
        KeyKind::Xorb => (root.join(XORBS_DIR), 2),
        KeyKind::Shard => (root.join(SHARDS_DIR), 2),
        KeyKind::Registration => (root.join(REGISTRATIONS_DIR), 4),
        KeyKind::XorbOwner => (root.join(XORB_OWNERS_DIR), 3),
    };
    let mut keys = Vec::new();
    for names in files_at_depth(&base, depth)? {
        let [dirs @ .., prefix, file] = names.as_slice() else {
            continue;
        };
        let Some(hash) = object_hash(prefix, file) else {
            continue;
        };
        let key = match (kind, dirs) {
            (KeyKind::Xorb, []) => Key::Xorb(hash),
            (KeyKind::Shard, []) => Key::Shard(hash),
            (KeyKind::Registration, [scope, tenant]) => {
                match (NameHash::from_hex(scope), NameHash::from_hex(tenant)) {
                    (Some(scope), Some(tenant)) => Key::Registration {
                        scope,
                        tenant,
                        shard: hash,
                    },
                    _ => continue,
                }
            }
            (KeyKind::XorbOwner, [tenant]) => match NameHash::from_hex(tenant) {
                Some(tenant) => Key::XorbOwner { tenant, xorb: hash },
                None => continue,
            },
            _ => continue,
        };
        keys.push(key);
    }
    Ok(keys)
}

/// The hash a file named `file` in prefix directory `prefix` stores, if the
/// names are the ones [`LocalBucket`] writes.
fn object_hash(prefix: &str, file: &str) -> Option<MerkleHash> {
    let hash = hash_from_hex(file).ok()?;
    (hash_to_hex(&hash) == file && file.get(..2) == Some(prefix)).then_some(hash)
}

/// The names leading from `base` to every regular file exactly `depth`
/// levels below it. Names that aren't UTF-8 are skipped: the bucket never
/// writes one.
fn files_at_depth(base: &Path, depth: usize) -> Result<Vec<Vec<String>>, StorageError> {
    let mut found = Vec::new();
    let mut pending = vec![(base.to_path_buf(), Vec::<String>::new())];
    while let Some((dir, names)) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            // Removed between listing its parent and here.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(io("list directory", &dir)(e)),
        };
        for entry in entries {
            let entry = entry.map_err(io("list directory", &dir))?;
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let file_type = entry.file_type().map_err(io("stat", &entry.path()))?;
            let mut path = names.clone();
            path.push(name);
            if path.len() == depth {
                if file_type.is_file() {
                    found.push(path);
                }
            } else if file_type.is_dir() {
                pending.push((entry.path(), path));
            }
        }
    }
    Ok(found)
}

/// Opens the object at `path` and checks `start..=end_inclusive` against its
/// size, returning the file and the size.
fn open_range(
    path: &Path,
    key: Key,
    start: u64,
    end_inclusive: u64,
) -> Result<(File, u64), StorageError> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StorageError::NotFound { key: Box::new(key) });
        }
        Err(e) => return Err(io("open", path)(e)),
    };
    let size = file.metadata().map_err(io("stat", path))?.len();
    if start > end_inclusive || end_inclusive >= size {
        return Err(StorageError::RangeOutOfBounds {
            key: Box::new(key),
            start,
            end_inclusive,
            size,
        });
    }
    Ok((file, size))
}

fn get_range_blocking(
    path: &Path,
    key: Key,
    start: u64,
    end_inclusive: u64,
) -> Result<Bytes, StorageError> {
    let (mut file, size) = open_range(path, key, start, end_inclusive)?;
    let Ok(len) = usize::try_from(end_inclusive - start + 1) else {
        return Err(StorageError::RangeOutOfBounds {
            key: Box::new(key),
            start,
            end_inclusive,
            size,
        });
    };
    file.seek(SeekFrom::Start(start))
        .map_err(io("seek", path))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(io("read", path))?;
    Ok(Bytes::from(buf))
}

/// Reads the next `n` bytes of `file`, from its position.
fn read_block(mut file: File, path: &Path, n: usize) -> (File, Result<Bytes, StorageError>) {
    let mut buf = vec![0u8; n];
    let block = file
        .read_exact(&mut buf)
        .map(|()| Bytes::from(buf))
        .map_err(io("read", path));
    (file, block)
}

/// A block read running on a blocking thread, which hands the file back.
type BlockRead = tokio::task::JoinHandle<(File, Result<Bytes, StorageError>)>;

/// A range read lazily, one [`STREAM_BLOCK_BYTES`] block per short
/// blocking task. While a block is handed out, the next is read ahead, so
/// at most two exist at once. No thread ever waits on the reader: a read
/// finishes on its own and its block waits in the task's result, so a
/// client that stops reading holds memory, not a blocking thread. Dropping
/// the stream drops the read ahead.
struct Blocks {
    path: Arc<Path>,
    /// The bytes not yet handed to a read.
    left: u64,
    /// The read in flight; `None` once the range is read or a read failed.
    read: Option<BlockRead>,
}

impl Blocks {
    fn new(file: File, path: PathBuf, len: u64) -> Self {
        let mut blocks = Self {
            path: path.into(),
            left: len,
            read: None,
        };
        blocks.read = blocks.next_read(file);
        blocks
    }

    /// Starts reading the next block of `file`, if any is left.
    fn next_read(&mut self, file: File) -> Option<BlockRead> {
        if self.left == 0 {
            return None;
        }
        // At most one block, which fits a usize.
        let n =
            usize::try_from(self.left.min(STREAM_BLOCK_BYTES as u64)).unwrap_or(STREAM_BLOCK_BYTES);
        self.left -= n as u64;
        let path = Arc::clone(&self.path);
        Some(tokio::task::spawn_blocking(move || {
            read_block(file, &path, n)
        }))
    }
}

impl futures_core::Stream for Blocks {
    type Item = Result<Bytes, StorageError>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::future::Future as _;
        use std::task::Poll;

        let Some(read) = self.read.as_mut() else {
            return Poll::Ready(None);
        };
        let done = match std::pin::Pin::new(read).poll(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(done) => done,
        };
        self.read = None;
        Poll::Ready(Some(match done {
            Ok((file, Ok(block))) => {
                self.read = self.next_read(file);
                Ok(block)
            }
            Ok((_, Err(e))) => Err(e),
            Err(e) => Err(join_error(&e)),
        }))
    }
}

fn join_error(e: &tokio::task::JoinError) -> StorageError {
    StorageError::Io {
        op: "run blocking storage task",
        path: PathBuf::new(),
        source: std::io::Error::other(e.to_string()),
    }
}

#[async_trait::async_trait]
impl Bucket for LocalBucket {
    async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError> {
        let (root, key, place) = (self.root.clone(), *key, self.place(key));
        let durable = Arc::clone(&self.fs);
        tokio::task::spawn_blocking(move || {
            put_blocking(durable.as_ref(), &root, key, &place, &bytes)
        })
        .await
        .map_err(|e| join_error(&e))?
    }

    async fn create(&self, key: &Key, bytes: Bytes) -> Result<bool, StorageError> {
        let (root, place) = (self.root.clone(), self.place(key));
        let durable = Arc::clone(&self.fs);
        tokio::task::spawn_blocking(move || {
            create_blocking(durable.as_ref(), &root, &place, &bytes)
        })
        .await
        .map_err(|e| join_error(&e))?
    }

    async fn list(&self, kind: KeyKind) -> Result<Vec<Key>, StorageError> {
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || list_blocking(&root, kind))
            .await
            .map_err(|e| join_error(&e))?
    }

    async fn get_range(
        &self,
        key: &Key,
        start: u64,
        end_inclusive: u64,
    ) -> Result<Bytes, StorageError> {
        let (key, path) = (*key, self.path(key));
        tokio::task::spawn_blocking(move || get_range_blocking(&path, key, start, end_inclusive))
            .await
            .map_err(|e| join_error(&e))?
    }

    async fn get_range_stream(
        &self,
        key: &Key,
        start: u64,
        end_inclusive: u64,
    ) -> Result<RangeStream, StorageError> {
        let (key, path) = (*key, self.path(key));
        let (file, size, path) = tokio::task::spawn_blocking(move || {
            let (mut file, size) = open_range(&path, key, start, end_inclusive)?;
            file.seek(SeekFrom::Start(start))
                .map_err(io("seek", &path))?;
            Ok::<_, StorageError>((file, size, path))
        })
        .await
        .map_err(|e| join_error(&e))??;
        Ok(RangeStream {
            size,
            blocks: Box::pin(Blocks::new(file, path, end_inclusive - start + 1)),
        })
    }

    async fn size(&self, key: &Key) -> Result<Option<u64>, StorageError> {
        let path = self.path(key);
        tokio::task::spawn_blocking(move || existing_size(&path))
            .await
            .map_err(|e| join_error(&e))?
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Mutex;

    use openxet_protocol::chunk_hash;

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Event {
        SyncFile,
        Rename,
        Link,
        SyncDir(PathBuf),
    }

    #[derive(Debug, Default)]
    struct Recorder(Mutex<Vec<Event>>);

    impl Recorder {
        fn take(&self) -> Vec<Event> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
    }

    #[derive(Debug)]
    struct Recording(Arc<Recorder>);

    impl Durability for Recording {
        fn sync_file(&self, file: &File) -> std::io::Result<()> {
            self.0.0.lock().unwrap().push(Event::SyncFile);
            RealFs.sync_file(file)
        }
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.0.0.lock().unwrap().push(Event::Rename);
            RealFs.rename(from, to)
        }
        fn hard_link(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            self.0.0.lock().unwrap().push(Event::Link);
            RealFs.hard_link(from, to)
        }
        fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
            self.0
                .0
                .lock()
                .unwrap()
                .push(Event::SyncDir(dir.to_path_buf()));
            RealFs.sync_dir(dir)
        }
    }

    /// A filesystem without hard links, as exFAT or some CIFS mounts are.
    #[derive(Debug)]
    struct NoLinks;

    impl Durability for NoLinks {
        fn sync_file(&self, file: &File) -> std::io::Result<()> {
            RealFs.sync_file(file)
        }
        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            RealFs.rename(from, to)
        }
        fn hard_link(&self, _: &Path, _: &Path) -> std::io::Result<()> {
            Err(std::io::Error::from(std::io::ErrorKind::Unsupported))
        }
        fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
            RealFs.sync_dir(dir)
        }
    }

    /// Records need hard links (`Bucket::create`), so a bucket on a
    /// filesystem without them refuses to open, saying so, instead of
    /// failing every shard upload later (F-A review m-4).
    #[test]
    fn open_refuses_a_filesystem_without_hard_links() {
        let dir = tempfile::tempdir().unwrap();
        let err = LocalBucket::open_with(dir.path(), Arc::new(NoLinks)).unwrap_err();
        assert!(matches!(err, StorageError::NoHardLinks { .. }), "{err:?}");
        assert!(err.to_string().contains("hard links"), "{err}");
        assert!(files_in(&dir.path().join(TMP_DIR)).is_empty());
    }

    fn recording_bucket(root: &Path) -> (LocalBucket, Arc<Recorder>) {
        let rec = Arc::new(Recorder::default());
        let bucket = LocalBucket::open_with(root, Arc::new(Recording(Arc::clone(&rec)))).unwrap();
        (bucket, rec)
    }

    fn sync_dirs(events: &[Event]) -> Vec<&Path> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::SyncDir(p) => Some(p.as_path()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn open_syncs_the_skeleton_and_root() {
        let dir = tempfile::tempdir().unwrap();
        let (_bucket, rec) = recording_bucket(dir.path());
        let events = rec.take();
        let r = dir.path();
        let synced = sync_dirs(&events);
        for expected in [
            r.join(".tmp"),
            r.join("shards"),
            r.join("xorbs/default"),
            r.join("registrations"),
            r.join("xorb-owners"),
            r.join("xorbs"),
            r.to_path_buf(),
        ] {
            assert!(
                synced.contains(&expected.as_path()),
                "{expected:?} not fsynced: {synced:?}"
            );
        }
        assert!(r.join("xorbs/default").is_dir() && r.join("shards").is_dir());
    }

    #[tokio::test]
    async fn put_into_a_fresh_prefix_syncs_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (bucket, rec) = recording_bucket(dir.path());
        rec.take();
        let key = Key::Xorb(chunk_hash(b"order"));
        bucket
            .put(&key, Bytes::from_static(b"payload"))
            .await
            .unwrap();

        let object_parent = bucket.path(&key).parent().unwrap().to_path_buf();
        let prefix_parent = object_parent.parent().unwrap().to_path_buf();
        assert_eq!(
            rec.take(),
            [
                Event::SyncFile,
                Event::SyncDir(prefix_parent),
                Event::Rename,
                Event::SyncDir(object_parent),
            ]
        );
    }

    #[tokio::test]
    async fn duplicate_put_syncs_directories_before_ok() {
        let dir = tempfile::tempdir().unwrap();
        let (bucket, rec) = recording_bucket(dir.path());
        let key = Key::Shard(chunk_hash(b"dup"));
        bucket
            .put(&key, Bytes::from_static(b"payload"))
            .await
            .unwrap();
        rec.take();
        bucket
            .put(&key, Bytes::from_static(b"payload"))
            .await
            .unwrap();

        let object_parent = bucket.path(&key).parent().unwrap().to_path_buf();
        let prefix_parent = object_parent.parent().unwrap().to_path_buf();
        assert_eq!(
            rec.take(),
            [Event::SyncDir(prefix_parent), Event::SyncDir(object_parent)]
        );
    }

    /// A record sits three directories below its skeleton directory, each
    /// new: every entry on the way is fsynced before the link, and the
    /// object's after it. A second `create` loses but syncs the same chain.
    #[tokio::test]
    async fn create_syncs_every_new_directory_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (bucket, rec) = recording_bucket(dir.path());
        rec.take();
        let key = Key::registration("scope", "tenant", chunk_hash(b"create"));
        assert!(
            bucket
                .create(&key, Bytes::from_static(b"first"))
                .await
                .unwrap()
        );

        let object_parent = bucket.path(&key).parent().unwrap().to_path_buf();
        let tenant_dir = object_parent.parent().unwrap().to_path_buf();
        let scope_dir = tenant_dir.parent().unwrap().to_path_buf();
        let base = dir.path().join(REGISTRATIONS_DIR);
        assert_eq!(scope_dir.parent().unwrap(), base);
        let chain = [
            Event::SyncDir(base),
            Event::SyncDir(scope_dir),
            Event::SyncDir(tenant_dir),
        ];
        let mut first = vec![Event::SyncFile];
        first.extend(chain.iter().cloned());
        first.extend([Event::Link, Event::SyncDir(object_parent.clone())]);
        assert_eq!(rec.take(), first);

        assert!(
            !bucket
                .create(&key, Bytes::from_static(b"second"))
                .await
                .unwrap()
        );
        let mut again = chain.to_vec();
        again.push(Event::SyncDir(object_parent));
        assert_eq!(rec.take(), again);
        assert!(files_in(&dir.path().join(TMP_DIR)).is_empty());
    }

    fn files_in(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }

    #[tokio::test]
    async fn put_into_an_existing_prefix_still_syncs_its_parent() {
        let dir = tempfile::tempdir().unwrap();
        let (bucket, rec) = recording_bucket(dir.path());
        let key = Key::Shard(chunk_hash(b"existing"));
        let object_parent = bucket.path(&key).parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&object_parent).unwrap();
        rec.take();
        bucket
            .put(&key, Bytes::from_static(b"payload"))
            .await
            .unwrap();

        let prefix_parent = object_parent.parent().unwrap().to_path_buf();
        let events = rec.take();
        assert!(
            sync_dirs(&events).contains(&prefix_parent.as_path()),
            "{events:?}"
        );
    }

    /// Every block of `stream`, read to the end.
    async fn blocks(mut stream: crate::RangeStream) -> Vec<Bytes> {
        let mut out = Vec::new();
        while let Some(block) =
            std::future::poll_fn(|cx| stream.blocks.as_mut().poll_next(cx)).await
        {
            out.push(block.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn range_stream_matches_get_range_in_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let bucket = LocalBucket::open(dir.path()).unwrap();
        let block = crate::STREAM_BLOCK_BYTES;
        let object: Vec<u8> = (0..3 * block + 17)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let size = object.len() as u64;
        let key = Key::Xorb(chunk_hash(b"stream"));
        bucket.put(&key, Bytes::from(object)).await.unwrap();

        let b = block as u64;
        for (start, end) in [
            (5, 5 + b - 2),       // under one block
            (0, b - 1),           // exactly one
            (3, 3 + 2 * b + 10),  // more than two
            (0, size - 1),        // the whole object
            (size - 1, size - 1), // its last byte
        ] {
            let stream = bucket.get_range_stream(&key, start, end).await.unwrap();
            assert_eq!(stream.size, size);
            let blocks = blocks(stream).await;
            assert!(blocks.iter().all(|b| !b.is_empty() && b.len() <= block));
            assert_eq!(
                blocks.concat(),
                bucket.get_range(&key, start, end).await.unwrap(),
                "{start}..={end}"
            );
        }
    }

    #[tokio::test]
    async fn range_stream_refuses_a_bad_range_before_any_block() {
        let dir = tempfile::tempdir().unwrap();
        let bucket = LocalBucket::open(dir.path()).unwrap();
        let key = Key::Xorb(chunk_hash(b"bounds"));
        bucket
            .put(&key, Bytes::from_static(b"0123456789"))
            .await
            .unwrap();

        for (start, end) in [(0, 10), (5, 4), (10, 10)] {
            assert!(matches!(
                bucket.get_range_stream(&key, start, end).await,
                Err(StorageError::RangeOutOfBounds { size: 10, .. })
            ));
        }
        let missing = Key::Xorb(chunk_hash(b"missing"));
        assert!(matches!(
            bucket.get_range_stream(&missing, 0, 0).await,
            Err(StorageError::NotFound { .. })
        ));
    }

    #[test]
    fn a_stalled_range_stream_holds_no_blocking_thread() {
        // A client that stops reading must not pin a blocking thread: the
        // pool also runs validation and every other bucket call.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let bucket = Arc::new(LocalBucket::open(dir.path()).unwrap());
            let key = Key::Xorb(chunk_hash(b"stalled"));
            let size = 8 * crate::STREAM_BLOCK_BYTES as u64;
            bucket
                .put(&key, Bytes::from(vec![7u8; 8 * crate::STREAM_BLOCK_BYTES]))
                .await
                .unwrap();

            // Two readers take one block each, then stop reading.
            let mut stalled = Vec::new();
            for _ in 0..2 {
                let mut stream = bucket.get_range_stream(&key, 0, size - 1).await.unwrap();
                let first = std::future::poll_fn(|cx| stream.blocks.as_mut().poll_next(cx)).await;
                assert_eq!(first.unwrap().unwrap().len(), crate::STREAM_BLOCK_BYTES);
                stalled.push(stream);
            }
            tokio::time::sleep(Duration::from_millis(200)).await;

            let other = tokio::time::timeout(Duration::from_secs(5), bucket.get_range(&key, 0, 9));
            assert_eq!(
                other
                    .await
                    .expect("the blocking pool is held by stalled readers")
                    .unwrap(),
                Bytes::from(vec![7u8; 10])
            );
            drop(stalled);
        });
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn directories_and_objects_are_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("bucket");
        let bucket = LocalBucket::open(&root).unwrap();
        bucket
            .put(&Key::Xorb(chunk_hash(b"private")), Bytes::from_static(b"x"))
            .await
            .unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let mut dirs = vec![root];
        let mut objects = 0;
        while let Some(dir) = dirs.pop() {
            assert_eq!(mode(&dir), 0o700, "{}", dir.display());
            for entry in fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    assert_eq!(mode(&path), 0o600, "{}", path.display());
                    objects += 1;
                }
            }
        }
        assert_eq!(objects, 1);
    }
}
