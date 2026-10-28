//! Disk space: the volume's free space, the free-space floor, and what a
//! directory takes up.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::WorkspaceError;

/// The smallest default floor: 20 GiB.
const MIN_FLOOR: u64 = 20 << 30;

/// A workspace with no activity for this long is idle.
pub const IDLE_AFTER: Duration = Duration::from_mins(10);

/// A volume's size and free space, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Volume {
    /// Bytes an unprivileged user may still write (`f_bavail`).
    pub free_bytes: u64,
    /// The volume's size.
    pub total_bytes: u64,
}

/// The volume `path` is on.
///
/// # Errors
///
/// [`WorkspaceError::Io`] if `statvfs` fails, for example for a missing
/// path.
pub fn volume(path: &Path) -> Result<Volume, WorkspaceError> {
    let stat = rustix::fs::statvfs(path).map_err(|err| WorkspaceError::Io {
        path: path.to_path_buf(),
        source: err.into(),
    })?;
    Ok(Volume {
        free_bytes: stat.f_bavail.saturating_mul(stat.f_frsize),
        total_bytes: stat.f_blocks.saturating_mul(stat.f_frsize),
    })
}

/// The default free-space floor for a volume of `total_bytes`: the larger
/// of 20 GiB and a tenth of the volume.
#[must_use]
pub fn default_floor(total_bytes: u64) -> u64 {
    MIN_FLOOR.max(total_bytes / 10)
}

/// Whether a workspace last active at `last_active` is idle at `now`.
#[must_use]
pub fn is_idle(last_active: SystemTime, now: SystemTime) -> bool {
    now.duration_since(last_active)
        .is_ok_and(|idle| idle >= IDLE_AFTER)
}

/// The bytes the files under `path` take on disk, as `du` counts them: a
/// file with several hard links once, and a clone in full, although it
/// shares its blocks until either copy changes. Entries it can't read are
/// left out; symbolic links are not followed.
#[must_use]
pub fn allocated_bytes(path: &Path) -> u64 {
    let mut seen = HashSet::new();
    let mut total = 0u64;
    walk(path, &mut |meta| {
        if meta.nlink() < 2 || seen.insert((meta.dev(), meta.ino())) {
            // `st_blocks` is in 512-byte units on every Unix
            // (https://pubs.opengroup.org/onlinepubs/9799919799/basedefs/sys_stat.h.html).
            total = total.saturating_add(meta.blocks().saturating_mul(512));
        }
    });
    total
}

/// Calls `visit` with the metadata of `path` and of everything under it,
/// without following symbolic links.
pub(crate) fn walk(path: &Path, visit: &mut impl FnMut(&std::fs::Metadata)) {
    let mut stack = vec![path.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&next) else {
            continue;
        };
        visit(&meta);
        if meta.is_dir()
            && let Ok(entries) = std::fs::read_dir(&next)
        {
            stack.extend(entries.filter_map(|entry| entry.ok().map(|e| e.path())));
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn floor_is_twenty_gib_or_a_tenth_of_the_volume() {
        assert_eq!(default_floor(0), 20 << 30);
        assert_eq!(default_floor(100 << 30), 20 << 30);
        assert_eq!(default_floor(1 << 40), (1 << 40) / 10);
    }

    #[test]
    fn idle_after_ten_minutes() {
        let now = SystemTime::now();
        assert!(!is_idle(now - Duration::from_mins(9), now));
        assert!(is_idle(now - Duration::from_mins(10), now));
        assert!(!is_idle(now + Duration::from_secs(1), now));
    }

    #[test]
    fn hard_links_count_once() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), vec![1u8; 64 << 10]).unwrap();
        let one = allocated_bytes(dir.path());
        std::fs::hard_link(dir.path().join("a"), dir.path().join("b")).unwrap();
        assert_eq!(allocated_bytes(dir.path()), one);
        assert!(one >= 64 << 10);
    }

    #[test]
    fn volume_reports_a_size() {
        let dir = tempfile::tempdir().unwrap();
        let here = volume(dir.path()).unwrap();
        assert!(here.total_bytes > 0 && here.free_bytes <= here.total_bytes);
        assert_eq!(
            volume(&dir.path().join("missing")).unwrap_err().code(),
            "io"
        );
    }
}
