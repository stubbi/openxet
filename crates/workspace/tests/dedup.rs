#![allow(clippy::unwrap_used)]

use std::fs::{self, File, FileTimes, Permissions};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use openxet_workspace::{DedupOutcome, Registry, dedup_file, dedup_tree};

const SETTLE: Duration = Duration::from_secs(2);

/// A time well past any settle window, with nanoseconds so a swap that
/// truncates timestamps is caught.
fn long_ago() -> SystemTime {
    UNIX_EPOCH + Duration::new(1_761_600_000, 123_456_789)
}

/// A canonical (symlink-free) temporary directory: the registry compares
/// paths byte for byte, and macOS's temporary directory is under the
/// `/var` symlink.
fn scratch() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

async fn registry() -> (tempfile::TempDir, Registry) {
    let state = tempfile::tempdir().unwrap();
    let registry = Registry::open(state.path()).await.unwrap();
    (state, registry)
}

/// Bytes that don't compress or repeat, so a filesystem can't store copies
/// cheaply by other means than sharing blocks.
fn content(len: usize, seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 32);
    let mut counter = 0u64;
    while out.len() < len {
        let mut hasher = blake3::Hasher::new();
        hasher.update(&seed.to_le_bytes());
        hasher.update(&counter.to_le_bytes());
        out.extend_from_slice(hasher.finalize().as_bytes());
        counter += 1;
    }
    out.truncate(len);
    out
}

/// Writes a settled file: its mtime and atime are [`long_ago`].
fn settled(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(
            FileTimes::new()
                .set_accessed(long_ago())
                .set_modified(long_ago()),
        )
        .unwrap();
}

fn hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

fn free_bytes(path: &Path) -> u64 {
    let stat = rustix::fs::statvfs(path).unwrap();
    stat.f_bavail * stat.f_frsize
}

fn reflink_supported(dir: &Path) -> bool {
    let from = dir.join("probe-from");
    let to = dir.join("probe-to");
    fs::write(&from, b"probe").unwrap();
    let supported = reflink_copy::reflink(&from, &to).is_ok();
    let _ = fs::remove_file(&from);
    let _ = fs::remove_file(&to);
    supported
}

/// Whether `dir` can clone files; if not, says the test is skipped. Never
/// skips on macOS, whose temporary directory is always on APFS, so the
/// clone assertions run there; a Linux runner's ext4 or tmpfs has no clones.
fn clones_or_skip(dir: &Path, test: &str) -> bool {
    if reflink_supported(dir) {
        return true;
    }
    assert_ne!(
        std::env::consts::OS,
        "macos",
        "{} can't clone files; macOS's temporary directory must be on APFS",
        dir.display()
    );
    eprintln!(
        "SKIPPED {test}: {} has no copy-on-write clones",
        dir.display()
    );
    false
}

fn no_leftover_temp_files(dir: &Path) {
    let names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(
        names
            .iter()
            .all(|name| !name.to_string_lossy().starts_with(".openxet-dedup")),
        "temporary files left in {}: {names:?}",
        dir.display()
    );
}

#[tokio::test]
async fn identical_files_share_blocks_after_dedup() {
    let (_dir, dir) = scratch();
    if !clones_or_skip(&dir, "identical_files_share_blocks_after_dedup") {
        return;
    }
    let (_state, reg) = registry().await;
    let size = 128 << 20;
    let bytes = content(size, 1);
    let [a, b, c] = ["a", "b", "c"].map(|name| dir.join(name));
    settled(&a, &bytes);
    settled(&b, &bytes);
    settled(&c, &bytes);

    assert_eq!(
        dedup_file(&reg, &a, SETTLE).await.unwrap(),
        DedupOutcome::Canonical
    );
    let size = u64::try_from(size).unwrap();
    assert_eq!(
        dedup_file(&reg, &b, SETTLE).await.unwrap(),
        DedupOutcome::Cloned { bytes: size }
    );

    let before = free_bytes(&dir);
    assert_eq!(
        dedup_file(&reg, &c, SETTLE).await.unwrap(),
        DedupOutcome::Cloned { bytes: size }
    );
    let after = free_bytes(&dir);
    // Half the file's size leaves room for other writers on a shared volume
    // while still failing if the swap kept a full copy.
    assert!(
        after >= before + size / 2,
        "deduping a {size} byte copy freed only {} bytes",
        after.saturating_sub(before)
    );
    for path in [&a, &b, &c] {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    assert_eq!(reg.canonical(size, hash(&bytes)).await.unwrap(), Some(a));
    no_leftover_temp_files(&dir);
}

#[tokio::test]
async fn swap_preserves_mtime_and_mode() {
    let (_dir, dir) = scratch();
    if !clones_or_skip(&dir, "swap_preserves_mtime_and_mode") {
        return;
    }
    let (_state, reg) = registry().await;
    let bytes = content(256 << 10, 2);
    let canon = dir.join("canon");
    let copy = dir.join("copy");
    settled(&canon, &bytes);
    fs::set_permissions(&canon, Permissions::from_mode(0o644)).unwrap();
    settled(&copy, &bytes);
    let accessed = long_ago() + Duration::new(5, 987_654_321);
    File::options()
        .write(true)
        .open(&copy)
        .unwrap()
        .set_times(FileTimes::new().set_accessed(accessed))
        .unwrap();
    // Read-only and executable, unlike the canonical copy: a swap that
    // inherits the source's mode would show here.
    fs::set_permissions(&copy, Permissions::from_mode(0o555)).unwrap();
    let inode_before = fs::metadata(&copy).unwrap().ino();

    assert_eq!(
        dedup_file(&reg, &canon, SETTLE).await.unwrap(),
        DedupOutcome::Canonical
    );
    assert_eq!(
        dedup_file(&reg, &copy, SETTLE).await.unwrap(),
        DedupOutcome::Cloned { bytes: 256 << 10 }
    );

    let meta = fs::symlink_metadata(&copy).unwrap();
    assert_ne!(meta.ino(), inode_before, "the file was not swapped");
    assert!(meta.file_type().is_file());
    assert_eq!(meta.modified().unwrap(), long_ago());
    assert_eq!(meta.accessed().unwrap(), accessed);
    assert_eq!(meta.permissions().mode() & 0o7777, 0o555);
    assert_eq!(fs::read(&copy).unwrap(), bytes);
    no_leftover_temp_files(&dir);
}

#[tokio::test]
async fn unsettled_files_are_skipped() {
    let (_dir, dir) = scratch();
    let (_state, reg) = registry().await;
    let bytes = content(256 << 10, 3);
    let canon = dir.join("canon");
    let fresh = dir.join("fresh");
    settled(&canon, &bytes);
    fs::write(&fresh, &bytes).unwrap();
    let inode = fs::metadata(&fresh).unwrap().ino();

    dedup_file(&reg, &canon, SETTLE).await.unwrap();
    assert_eq!(
        dedup_file(&reg, &fresh, Duration::from_hours(1))
            .await
            .unwrap(),
        DedupOutcome::Unsettled
    );
    assert_eq!(fs::metadata(&fresh).unwrap().ino(), inode);
    assert_eq!(
        reg.canonical(256 << 10, hash(&bytes)).await.unwrap(),
        Some(canon)
    );
}

#[tokio::test]
async fn small_files_are_skipped() {
    let (_dir, dir) = scratch();
    let (_state, reg) = registry().await;
    let small = content((64 << 10) - 1, 4);
    let path = dir.join("small");
    settled(&path, &small);
    assert_eq!(
        dedup_file(&reg, &path, SETTLE).await.unwrap(),
        DedupOutcome::TooSmall
    );
    assert_eq!(
        reg.canonical((64 << 10) - 1, hash(&small)).await.unwrap(),
        None
    );

    let threshold = content(64 << 10, 4);
    let path = dir.join("threshold");
    settled(&path, &threshold);
    assert_eq!(
        dedup_file(&reg, &path, SETTLE).await.unwrap(),
        DedupOutcome::Canonical
    );
}

#[tokio::test]
async fn stale_canonical_is_replaced() {
    let (_dir, dir) = scratch();
    let (_state, reg) = registry().await;
    let bytes = content(256 << 10, 5);
    let size = 256 << 10;

    // Rewritten in place since it was indexed: same size, other bytes.
    let rewritten = dir.join("rewritten");
    let copy = dir.join("copy");
    settled(&rewritten, &bytes);
    settled(&copy, &bytes);
    dedup_file(&reg, &rewritten, SETTLE).await.unwrap();
    settled(&rewritten, &content(size, 6));
    let inode = fs::metadata(&copy).unwrap().ino();
    assert_eq!(
        dedup_file(&reg, &copy, SETTLE).await.unwrap(),
        DedupOutcome::Canonical
    );
    assert_eq!(fs::metadata(&copy).unwrap().ino(), inode);
    assert_eq!(fs::read(&copy).unwrap(), bytes);
    assert_eq!(fs::read(&rewritten).unwrap(), content(size, 6));
    let size = u64::try_from(size).unwrap();
    assert_eq!(
        reg.canonical(size, hash(&bytes)).await.unwrap(),
        Some(copy.clone())
    );

    // Deleted since it was indexed.
    let other = dir.join("other");
    settled(&other, &bytes);
    fs::remove_file(&copy).unwrap();
    assert_eq!(
        dedup_file(&reg, &other, SETTLE).await.unwrap(),
        DedupOutcome::Canonical
    );
    assert_eq!(
        reg.canonical(size, hash(&bytes)).await.unwrap(),
        Some(other)
    );
    no_leftover_temp_files(&dir);
}

/// A mounted HFS+ disk image, which has no clones, detached on drop.
#[cfg(target_os = "macos")]
struct NoReflinkVolume {
    _image_dir: tempfile::TempDir,
    mountpoint: PathBuf,
}

#[cfg(target_os = "macos")]
impl NoReflinkVolume {
    fn mount() -> Option<Self> {
        let image_dir = tempfile::tempdir().unwrap();
        let image = image_dir.path().join("hfs.dmg");
        let mountpoint = image_dir.path().join("mnt");
        fs::create_dir(&mountpoint).unwrap();
        let created = Command::new("hdiutil")
            .args([
                "create", "-quiet", "-size", "8m", "-fs", "HFS+", "-layout", "NONE",
            ])
            .args(["-volname", "openxet-no-reflink"])
            .arg(&image)
            .status()
            .ok()?
            .success();
        let attached = created
            && Command::new("hdiutil")
                .args(["attach", "-quiet", "-nobrowse", "-noverify", "-mountpoint"])
                .arg(&mountpoint)
                .arg(&image)
                .status()
                .ok()?
                .success();
        attached.then(|| Self {
            mountpoint: mountpoint.canonicalize().unwrap(),
            _image_dir: image_dir,
        })
    }
}

#[cfg(target_os = "macos")]
impl Drop for NoReflinkVolume {
    fn drop(&mut self) {
        let _ = Command::new("hdiutil")
            .args(["detach", "-quiet", "-force"])
            .arg(&self.mountpoint)
            .status();
    }
}

#[tokio::test]
async fn dedup_without_reflink_support_reports_unsupported() {
    #[cfg(target_os = "macos")]
    let volume = NoReflinkVolume::mount();
    #[cfg(target_os = "macos")]
    let candidates: Vec<PathBuf> = volume.iter().map(|v| v.mountpoint.clone()).collect();
    #[cfg(not(target_os = "macos"))]
    let candidates: Vec<PathBuf> = [std::env::temp_dir(), PathBuf::from("/dev/shm")]
        .into_iter()
        .filter(|dir| dir.is_dir())
        .collect();
    let mut dirs = Vec::new();
    for parent in candidates {
        let dir = tempfile::tempdir_in(&parent).unwrap();
        if !reflink_supported(dir.path()) {
            dirs.push(dir);
        }
    }
    let Some(dir) = dirs.first() else {
        eprintln!(
            "SKIPPED dedup_without_reflink_support_reports_unsupported: no volume without \
             copy-on-write clones is available"
        );
        return;
    };
    let dir = dir.path().canonicalize().unwrap();
    let (_state, reg) = registry().await;
    let bytes = content(1 << 20, 7);
    let canon = dir.join("canon");
    let copy = dir.join("copy");
    settled(&canon, &bytes);
    settled(&copy, &bytes);
    // HFS+ keeps whole seconds, so compare with what it stored.
    let before = fs::metadata(&copy).unwrap();

    assert_eq!(
        dedup_file(&reg, &canon, SETTLE).await.unwrap(),
        DedupOutcome::Canonical
    );
    assert_eq!(
        dedup_file(&reg, &copy, SETTLE).await.unwrap(),
        DedupOutcome::Unsupported
    );
    // Never a byte copy in its place: the file is the same inode.
    let meta = fs::metadata(&copy).unwrap();
    assert_eq!(meta.ino(), before.ino());
    assert_eq!(meta.modified().unwrap(), before.modified().unwrap());
    assert_eq!(fs::read(&copy).unwrap(), bytes);
    no_leftover_temp_files(&dir);

    let report = dedup_tree(&reg, &dir, SETTLE).await;
    assert!(report.unsupported);
    assert_eq!(report.cloned_bytes, 0);
}

#[tokio::test]
async fn dedup_tree_clones_regular_files_and_skips_symlinks() {
    let (_dir, dir) = scratch();
    if !clones_or_skip(&dir, "dedup_tree_clones_regular_files_and_skips_symlinks") {
        return;
    }
    let (_state, reg) = registry().await;
    let bytes = content(256 << 10, 8);
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().canonicalize().unwrap().join("outside");
    settled(&outside_file, &bytes);
    let outside_inode = fs::metadata(&outside_file).unwrap().ino();
    fs::create_dir_all(dir.join("one/deps")).unwrap();
    fs::create_dir_all(dir.join("two/deps")).unwrap();
    settled(&dir.join("one/deps/libx.rlib"), &bytes);
    settled(&dir.join("two/deps/libx.rlib"), &bytes);
    settled(&dir.join("two/small"), b"small");
    std::os::unix::fs::symlink(&outside_file, dir.join("two/link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.join("two/linked-dir")).unwrap();

    let report = dedup_tree(&reg, &dir, SETTLE).await;
    assert_eq!(report.files_seen, 3);
    assert_eq!(report.cloned_bytes, 256 << 10);
    assert!(!report.unsupported);
    assert_eq!(report.failed, 0);
    assert_eq!(fs::metadata(&outside_file).unwrap().ino(), outside_inode);
    assert!(
        fs::symlink_metadata(dir.join("two/link"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
