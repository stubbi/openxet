#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use openxet_protocol::{MerkleHash, chunk_hash, hash_to_hex};
use openxet_storage::{Bucket, Key, LocalBucket, StorageError};

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

fn payload(n: usize) -> Bytes {
    Bytes::from(
        (0..n)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect::<Vec<u8>>(),
    )
}

fn files_under(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(files_under(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[tokio::test]
async fn put_then_get_range() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let key = Key::Xorb(h("a"));
    let data = payload(1000);
    bucket.put(&key, data.clone()).await.unwrap();

    assert_eq!(bucket.size(&key).await.unwrap(), Some(1000));
    assert_eq!(
        bucket.get_range(&key, 0, 0).await.unwrap(),
        data.slice(0..1)
    );
    assert_eq!(
        bucket.get_range(&key, 999, 999).await.unwrap(),
        data.slice(999..1000)
    );
    assert_eq!(bucket.get_range(&key, 0, 999).await.unwrap(), data);
    assert_eq!(
        bucket.get_range(&key, 10, 19).await.unwrap(),
        data.slice(10..20)
    );

    let shard = Key::Shard(h("a"));
    bucket.put(&shard, data.clone()).await.unwrap();
    assert_eq!(bucket.get_range(&shard, 0, 999).await.unwrap(), data);
}

#[test]
fn logical_keys_are_the_documented_strings() {
    let hash = h("k");
    let hex = hash_to_hex(&hash);
    assert_eq!(Key::Xorb(hash).to_string(), format!("xorbs/default/{hex}"));
    assert_eq!(Key::Shard(hash).to_string(), format!("shards/{hex}"));
}

#[tokio::test]
async fn put_is_atomic_under_concurrency() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(LocalBucket::open(dir.path()).unwrap());
    let key = Key::Xorb(h("c"));
    let data = payload(256 * 1024);
    let tasks: Vec<_> = (0..16)
        .map(|_| {
            let (b, d) = (Arc::clone(&bucket), data.clone());
            tokio::spawn(async move { b.put(&key, d).await })
        })
        .collect();
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    assert_eq!(
        bucket
            .get_range(&key, 0, data.len() as u64 - 1)
            .await
            .unwrap(),
        data
    );
    assert!(
        files_under(&dir.path().join(".tmp")).is_empty(),
        "temp files left behind"
    );
    assert_eq!(files_under(dir.path()).len(), 1, "exactly one object");
}

#[tokio::test]
async fn put_is_idempotent_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let key = Key::Shard(h("i"));
    bucket.put(&key, payload(100)).await.unwrap();
    bucket.put(&key, payload(100)).await.unwrap();

    let err = bucket.put(&key, payload(101)).await.unwrap_err();
    assert!(
        matches!(
            err,
            StorageError::SizeConflict {
                existing: 100,
                new: 101,
                ..
            }
        ),
        "{err:?}"
    );
    assert_eq!(bucket.size(&key).await.unwrap(), Some(100));
}

#[tokio::test]
async fn range_past_end_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let key = Key::Xorb(h("r"));
    bucket.put(&key, payload(10)).await.unwrap();
    for (s, e) in [(0, 10), (5, 100), (10, 10), (5, 4)] {
        let err = bucket.get_range(&key, s, e).await.unwrap_err();
        assert!(
            matches!(err, StorageError::RangeOutOfBounds { size: 10, .. }),
            "{err:?}"
        );
    }
}

#[tokio::test]
async fn missing_key_is_none() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = LocalBucket::open(dir.path()).unwrap();
    let key = Key::Xorb(h("missing"));
    assert_eq!(bucket.size(&key).await.unwrap(), None);
    let err = bucket.get_range(&key, 0, 0).await.unwrap_err();
    assert!(matches!(err, StorageError::NotFound { .. }), "{err:?}");
}

#[test]
fn open_removes_only_stale_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join(".tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let stale = tmp.join("stale");
    let fresh = tmp.join("fresh");
    std::fs::write(&stale, b"x").unwrap();
    std::fs::write(&fresh, b"x").unwrap();
    let two_hours_ago = SystemTime::now() - Duration::from_hours(2);
    std::fs::File::options()
        .write(true)
        .open(&stale)
        .unwrap()
        .set_modified(two_hours_ago)
        .unwrap();

    LocalBucket::open(dir.path()).unwrap();
    assert!(!stale.exists());
    assert!(fresh.exists());
}

#[tokio::test]
async fn bucket_works_as_a_trait_object() {
    let dir = tempfile::tempdir().unwrap();
    let bucket: Arc<dyn Bucket> = Arc::new(LocalBucket::open(dir.path()).unwrap());
    bucket.put(&Key::Xorb(h("d")), payload(3)).await.unwrap();
}

const CHILD_ROOT: &str = "OPENXET_STORAGE_KILL_CHILD_ROOT";

/// Child role of `put_survives_process_kill`: does nothing unless spawned by
/// the parent test with the env var set.
#[tokio::test]
async fn kill_child() {
    let Ok(root) = std::env::var(CHILD_ROOT) else {
        return;
    };
    let bucket = LocalBucket::open(Path::new(&root)).unwrap();
    bucket
        .put(&Key::Xorb(h("kill")), payload(100_000))
        .await
        .unwrap();
    println!("\nput-acknowledged");
    let pid = std::process::id().to_string();
    Command::new("kill").args(["-9", &pid]).status().unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

// SIGKILL proves the data is not held only in this process's memory or
// unflushed userspace buffers. It does not prove survival of power loss: that
// is the job of the `sync_all` on the temp file and the fsync of the parent
// directory in `LocalBucket::put`, which are checked in code review.
#[tokio::test]
async fn put_survives_process_kill() {
    if std::env::var(CHILD_ROOT).is_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "kill_child", "--nocapture", "--test-threads=1"])
        .env(CHILD_ROOT, dir.path())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let acked = out
        .lines()
        .map(Result::unwrap)
        .any(|l| l == "put-acknowledged");
    let status = child.wait().unwrap();
    assert!(acked, "child never acknowledged the put");
    assert!(!status.success(), "child should have been killed");
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "child must die by SIGKILL, got {status:?}"
        );
    }

    let bucket = LocalBucket::open(dir.path()).unwrap();
    let key = Key::Xorb(h("kill"));
    assert_eq!(
        bucket.get_range(&key, 0, 99_999).await.unwrap(),
        payload(100_000)
    );
}
