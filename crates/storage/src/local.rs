//! Local-disk bucket.
//!
//! On-disk layout under the root (what `openxet reindex` walks, issue #26):
//!
//! ```text
//! {root}/xorbs/default/{hex[0..2]}/{hex}   Key::Xorb
//! {root}/shards/{hex[0..2]}/{hex}          Key::Shard
//! {root}/.tmp/{uuid}                       in-flight writes
//! ```
//!
//! `hex` is [`hash_to_hex`]. The two-character prefix directory keeps any one
//! directory from growing unbounded. `.tmp` lives under the root so the final
//! rename never crosses a filesystem and stays atomic.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use openxet_protocol::hash_to_hex;

use crate::{Bucket, Key, StorageError};

const TMP_DIR: &str = ".tmp";
/// Temp files older than this are leftovers from a crash; no healthy write
/// stays in flight for an hour.
const STALE_TEMP_AGE: Duration = Duration::from_hours(1);

/// The filesystem calls that make a write durable, behind a seam so tests can
/// observe that each one happens and in which order. Crate-private.
pub(crate) trait Durability: Send + Sync + std::fmt::Debug + 'static {
    fn sync_file(&self, file: &File) -> std::io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
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
    /// Opens (creating if needed) the bucket at `root` and deletes temp files
    /// older than one hour left behind by crashed writes.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the directories cannot be created or listed.
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        Self::open_with(root, Arc::new(RealFs))
    }

    pub(crate) fn open_with(root: &Path, fs: Arc<dyn Durability>) -> Result<Self, StorageError> {
        let tmp = root.join(TMP_DIR);
        let xorbs = root.join("xorbs");
        let skeleton = [tmp.clone(), root.join("shards"), xorbs.join("default")];
        for dir in &skeleton {
            fs::create_dir_all(dir).map_err(io("create bucket directory", dir))?;
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
        Ok(Self {
            root: root.to_path_buf(),
            fs,
        })
    }

    fn path(&self, key: &Key) -> PathBuf {
        let (dir, hash) = match key {
            Key::Xorb(h) => (self.root.join("xorbs").join("default"), h),
            Key::Shard(h) => (self.root.join("shards"), h),
        };
        let hex = hash_to_hex(hash);
        dir.join(&hex[..2]).join(hex)
    }
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

/// Makes the object's directory entries durable: the prefix directory in its
/// parent, and the object in the prefix directory. Whichever put created
/// them, no put returns `Ok` before they are on disk.
fn sync_object_dirs(durable: &dyn Durability, final_path: &Path) -> Result<(), StorageError> {
    let prefix_dir = final_path.parent().unwrap_or(final_path);
    if let Some(grandparent) = prefix_dir.parent() {
        fsync_dir(durable, grandparent)?;
    }
    fsync_dir(durable, prefix_dir)
}

fn put_blocking(
    durable: &dyn Durability,
    root: &Path,
    key: Key,
    final_path: &Path,
    bytes: &[u8],
) -> Result<(), StorageError> {
    let new = bytes.len() as u64;
    let check = |existing: u64| {
        if existing == new {
            Ok(())
        } else {
            Err(StorageError::SizeConflict { key, existing, new })
        }
    };
    if let Some(existing) = existing_size(final_path)? {
        check(existing)?;
        // The object may come from a concurrent put that has not finished its
        // fsyncs; our Ok must not outrun them.
        return sync_object_dirs(durable, final_path);
    }

    let tmp_path = root
        .join(TMP_DIR)
        .join(uuid::Uuid::new_v4().simple().to_string());
    let result = write_and_publish(durable, &tmp_path, final_path, bytes);
    if result.is_err() {
        // Best effort: a leftover is also swept by the next `open`.
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn write_and_publish(
    durable: &dyn Durability,
    tmp_path: &Path,
    final_path: &Path,
    bytes: &[u8],
) -> Result<(), StorageError> {
    let mut file = File::create_new(tmp_path).map_err(io("create temp file", tmp_path))?;
    file.write_all(bytes)
        .map_err(io("write temp file", tmp_path))?;
    durable
        .sync_file(&file)
        .map_err(io("fsync temp file", tmp_path))?;
    drop(file);

    let prefix_dir = final_path.parent().unwrap_or(final_path);
    fs::create_dir_all(prefix_dir).map_err(io("create directory", prefix_dir))?;
    // Before the rename, so a crash never leaves the object under a prefix
    // directory whose own entry was lost.
    if let Some(grandparent) = prefix_dir.parent() {
        fsync_dir(durable, grandparent)?;
    }
    durable
        .rename(tmp_path, final_path)
        .map_err(io("rename into place", final_path))?;
    fsync_dir(durable, prefix_dir)?;
    Ok(())
}

fn get_range_blocking(
    path: &Path,
    key: Key,
    start: u64,
    end_inclusive: u64,
) -> Result<Bytes, StorageError> {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(StorageError::NotFound { key });
        }
        Err(e) => return Err(io("open", path)(e)),
    };
    let size = file.metadata().map_err(io("stat", path))?.len();
    let out_of_bounds = StorageError::RangeOutOfBounds {
        key,
        start,
        end_inclusive,
        size,
    };
    if start > end_inclusive || end_inclusive >= size {
        return Err(out_of_bounds);
    }
    let Ok(len) = usize::try_from(end_inclusive - start + 1) else {
        return Err(out_of_bounds);
    };
    file.seek(SeekFrom::Start(start))
        .map_err(io("seek", path))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf).map_err(io("read", path))?;
    Ok(Bytes::from(buf))
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
        let (root, key, path) = (self.root.clone(), *key, self.path(key));
        let durable = Arc::clone(&self.fs);
        tokio::task::spawn_blocking(move || {
            put_blocking(durable.as_ref(), &root, key, &path, &bytes)
        })
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

    #[derive(Debug, PartialEq, Eq)]
    enum Event {
        SyncFile,
        Rename,
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
        fn sync_dir(&self, dir: &Path) -> std::io::Result<()> {
            self.0
                .0
                .lock()
                .unwrap()
                .push(Event::SyncDir(dir.to_path_buf()));
            RealFs.sync_dir(dir)
        }
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
}
