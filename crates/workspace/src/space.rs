//! Disk space: the volume's free space, the disk budget (floor and idle
//! TTL) and what a directory takes up.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
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

/// The default [`Budget::idle_ttl`]: a day (plan 2026-10-27, Global
/// Constraints).
const DEFAULT_IDLE_TTL: Duration = Duration::from_hours(24);

/// The default [`Budget::settle`].
const DEFAULT_SETTLE: Duration = Duration::from_secs(2);

/// How much disk the workspaces may use, and when their build outputs may
/// go: the human's limits, from [`user_config_path`] over the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// The free space to keep on every volume that holds a workspace, or
    /// `None` for [`default_floor`] of each volume. A volume's size differs
    /// from the next one's, so the default is per volume.
    pub floor_bytes: Option<u64>,
    /// A workspace with no activity for this long loses its reconstructible
    /// directories.
    pub idle_ttl: Duration,
    /// A file unmodified for this long is done being written and may be
    /// deduplicated.
    pub settle: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            floor_bytes: None,
            idle_ttl: DEFAULT_IDLE_TTL,
            settle: DEFAULT_SETTLE,
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UserConfig {
    floor: Option<Size>,
    idle_ttl: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Size {
    Bytes(u64),
    Text(String),
}

impl Budget {
    /// The floor for `volume`.
    #[must_use]
    pub fn floor_for(&self, volume: Volume) -> u64 {
        self.floor_bytes
            .unwrap_or_else(|| default_floor(volume.total_bytes))
    }

    /// The defaults, with the keys `floor` (bytes, or a size such as
    /// `"50 GiB"`) and `idle_ttl` (such as `"90m"` or `"24h"`) of the TOML
    /// file at `path` over them. A missing file means the defaults.
    ///
    /// # Errors
    ///
    /// [`WorkspaceError::Config`] if the file is not valid TOML, has another
    /// key, or a value that doesn't parse; [`WorkspaceError::Io`] if it
    /// exists but can't be read.
    pub fn load(path: &Path) -> Result<Self, WorkspaceError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(err) => return Err(WorkspaceError::io(path)(err)),
        };
        let invalid = |message: String| WorkspaceError::Config {
            path: path.to_path_buf(),
            message,
        };
        let config: UserConfig = toml::from_str(&text).map_err(|err| {
            invalid(format!(
                "{}; the keys are `floor` and `idle_ttl`",
                err.message()
            ))
        })?;
        let mut budget = Self::default();
        if let Some(floor) = config.floor {
            budget.floor_bytes = Some(match floor {
                Size::Bytes(bytes) => bytes,
                Size::Text(text) => parse_size(&text).ok_or_else(|| {
                    invalid(format!(
                        "`floor = {text:?}` is not a size; write bytes, or a whole number with \
                         B, KiB, MiB, GiB or TiB, such as \"50 GiB\""
                    ))
                })?,
            });
        }
        if let Some(ttl) = config.idle_ttl {
            budget.idle_ttl = parse_duration(&ttl).ok_or_else(|| {
                invalid(format!(
                    "`idle_ttl = {ttl:?}` is not a duration; write a whole number over 0 with s, \
                     m, h or d, such as \"24h\""
                ))
            })?;
        }
        Ok(budget)
    }
}

/// The human's workspace limits: `$XDG_CONFIG_HOME/openxet/workspace.toml`,
/// by default `~/.config/openxet/workspace.toml`, on macOS too, where
/// command-line tools keep their configuration there as well. A relative
/// `XDG_CONFIG_HOME` is ignored, as the XDG Base Directory spec requires
/// (<https://specifications.freedesktop.org/basedir-spec/latest/>). `None`
/// without a home directory.
#[must_use]
pub fn user_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("openxet/workspace.toml"))
}

/// `"50 GiB"`, `"50GiB"` or `"4096 B"` in bytes.
fn parse_size(text: &str) -> Option<u64> {
    let (number, unit) = split_number(text)?;
    let shift = match unit {
        "B" => 0,
        "KiB" => 10,
        "MiB" => 20,
        "GiB" => 30,
        "TiB" => 40,
        _ => return None,
    };
    number.checked_mul(1 << shift)
}

/// `"90m"` or `"24 h"`, over zero.
fn parse_duration(text: &str) -> Option<Duration> {
    let (number, unit) = split_number(text)?;
    let seconds = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 60 * 60,
        "d" => 24 * 60 * 60,
        _ => return None,
    };
    let seconds = number.checked_mul(seconds)?;
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

fn split_number(text: &str) -> Option<(u64, &str)> {
    let text = text.trim();
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let number = text[..digits].parse().ok()?;
    Some((number, text[digits..].trim_start()))
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
