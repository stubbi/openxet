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
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use openxet_protocol::hash_to_hex;

use crate::{Bucket, Key, StorageError};

const TMP_DIR: &str = ".tmp";
/// Temp files older than this are leftovers from a crash; no healthy write
/// stays in flight for an hour.
const STALE_TEMP_AGE: Duration = Duration::from_hours(1);

/// A [`Bucket`] on a local filesystem. Cheap to clone-share behind an `Arc`.
#[derive(Debug)]
pub struct LocalBucket {
    root: PathBuf,
}

impl LocalBucket {
    /// Opens (creating if needed) the bucket at `root` and deletes temp files
    /// older than one hour left behind by crashed writes.
    ///
    /// # Errors
    ///
    /// [`StorageError::Io`] if the directories cannot be created or listed.
    pub fn open(root: &Path) -> Result<Self, StorageError> {
        let tmp = root.join(TMP_DIR);
        fs::create_dir_all(&tmp).map_err(io("create bucket directory", &tmp))?;
        sweep_stale_temp(&tmp)?;
        Ok(Self {
            root: root.to_path_buf(),
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

fn fsync_dir(dir: &Path) -> Result<(), StorageError> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io("fsync directory", dir))
}

fn existing_size(path: &Path) -> Result<Option<u64>, StorageError> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some(m.len())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io("stat", path)(e)),
    }
}

fn put_blocking(
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
        return check(existing);
    }

    let tmp_path = root
        .join(TMP_DIR)
        .join(uuid::Uuid::new_v4().simple().to_string());
    let result = write_and_publish(&tmp_path, final_path, bytes);
    if result.is_err() {
        // Best effort: a leftover is also swept by the next `open`.
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn write_and_publish(tmp_path: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    let mut file = File::create_new(tmp_path).map_err(io("create temp file", tmp_path))?;
    file.write_all(bytes)
        .map_err(io("write temp file", tmp_path))?;
    file.sync_all().map_err(io("fsync temp file", tmp_path))?;
    drop(file);

    // `final_path` always has a parent: it is `{root}/.../{prefix}/{hex}`.
    let prefix_dir = final_path.parent().unwrap_or(final_path);
    let prefix_existed = prefix_dir.exists();
    fs::create_dir_all(prefix_dir).map_err(io("create directory", prefix_dir))?;

    fs::rename(tmp_path, final_path).map_err(io("rename into place", final_path))?;
    fsync_dir(prefix_dir)?;
    if !prefix_existed && let Some(grandparent) = prefix_dir.parent() {
        // The new prefix directory's own entry must be durable too.
        fsync_dir(grandparent)?;
    }
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
        tokio::task::spawn_blocking(move || put_blocking(&root, key, &path, &bytes))
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
