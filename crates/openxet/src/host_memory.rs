//! The memory this process may use, for the default `server.memory_budget`
//! (ruling M1-R40): the host's physical memory, or, on Linux, the
//! container's cgroup limit when that is lower.
//!
//! Read the simplest way each system offers, with no `unsafe`:
//! - Linux: `MemTotal` in `/proc/meminfo`, and the cgroup v2 `memory.max`
//!   or v1 `memory.limit_in_bytes` of the process's cgroup root
//! - macOS: `sysctl -n hw.memsize`
//!
//! Anything else, or a read that fails, gives `None`, and the budget falls
//! back to its floor.

/// The memory this process may use, in bytes, if it can be read.
pub fn total() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let physical = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| meminfo_total(&text));
        let limit = [
            "/sys/fs/cgroup/memory.max",
            "/sys/fs/cgroup/memory/memory.limit_in_bytes",
        ]
        .iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| cgroup_limit(&text));
        match (physical, limit) {
            (Some(p), Some(l)) => Some(p.min(l)),
            (p, l) => p.or(l),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
            .flatten()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// `MemTotal` from the text of `/proc/meminfo`, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn meminfo_total(text: &str) -> Option<u64> {
    let rest = text.lines().find_map(|l| l.strip_prefix("MemTotal:"))?;
    let mut words = rest.split_whitespace();
    let kib: u64 = words.next()?.parse().ok()?;
    (words.next() == Some("kB")).then_some(kib.checked_mul(1024)?)
}

/// A cgroup memory limit, in bytes: `None` for v2's `max`, and for v1's
/// "no limit", a value near `i64::MAX`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cgroup_limit(text: &str) -> Option<u64> {
    let limit: u64 = text.trim().parse().ok()?;
    (limit < 1 << 60).then_some(limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_meminfo_total() {
        let text = "MemTotal:       16303312 kB\nMemFree:         1234 kB\n";
        assert_eq!(meminfo_total(text), Some(16_303_312 * 1024));
        assert_eq!(meminfo_total("MemFree: 1 kB\n"), None);
        assert_eq!(meminfo_total("MemTotal: x kB\n"), None);
    }

    #[test]
    fn reads_cgroup_limits() {
        assert_eq!(cgroup_limit("2147483648\n"), Some(2 << 30));
        assert_eq!(cgroup_limit("max\n"), None);
        assert_eq!(cgroup_limit("9223372036854771712\n"), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn this_host_has_some_memory() {
        let total = total().unwrap();
        assert!(total >= 256 << 20, "{total}");
    }
}
