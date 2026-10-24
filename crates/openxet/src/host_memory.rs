//! The memory this process may use, for the default `server.memory_budget`
//! (ruling M1-R40): the host's physical memory, or, on Linux, the
//! container's cgroup limit when that is lower.
//!
//! Read the simplest way each system offers, with no `unsafe`:
//! - Linux: `MemTotal` in `/proc/meminfo`, lowered to the smallest cgroup
//!   v2 `memory.max` (or v1 `memory.limit_in_bytes`) from the process's own
//!   cgroup, named in `/proc/self/cgroup`, up to the root. So a container's
//!   limit counts, and so does a systemd unit's `MemoryMax=` or a cgroup
//!   seen from the host's namespace (review M-2 of ruling M1-R40).
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
        let limit = std::fs::read_to_string("/proc/self/cgroup")
            .map(|text| cgroup_limit_files(&text))
            .unwrap_or_default()
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .filter_map(|text| cgroup_limit(&text))
            .min();
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

/// The files that may hold a memory limit for the cgroups named in the
/// text of `/proc/self/cgroup`, from the process's own cgroup up to the
/// root: `memory.max` for cgroup v2 (`0::/path`), `memory.limit_in_bytes`
/// for a v1 hierarchy with the `memory` controller (`N:memory:/path`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn cgroup_limit_files(text: &str) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for line in text.lines() {
        let mut fields = line.splitn(3, ':');
        let (Some(_), Some(controllers), Some(path)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (mount, file) = if controllers.is_empty() {
            ("/sys/fs/cgroup", "memory.max")
        } else if controllers.split(',').any(|c| c == "memory") {
            ("/sys/fs/cgroup/memory", "memory.limit_in_bytes")
        } else {
            continue;
        };
        let mut dir = std::path::Path::new(mount).join(path.trim_start_matches('/'));
        loop {
            files.push(dir.join(file));
            if dir == std::path::Path::new(mount) || !dir.pop() {
                break;
            }
        }
    }
    files
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
    fn finds_the_limit_files_from_the_process_cgroup_up() {
        let v2 = cgroup_limit_files("0::/system.slice/openxet.service\n");
        let v2: Vec<_> = v2.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            v2,
            [
                "/sys/fs/cgroup/system.slice/openxet.service/memory.max",
                "/sys/fs/cgroup/system.slice/memory.max",
                "/sys/fs/cgroup/memory.max",
            ]
        );
        assert_eq!(
            cgroup_limit_files("0::/\n"),
            [std::path::PathBuf::from("/sys/fs/cgroup/memory.max")]
        );
        let v1 = cgroup_limit_files("5:cpu,cpuacct:/docker/x\n4:memory:/docker/x\n");
        let v1: Vec<_> = v1.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            v1,
            [
                "/sys/fs/cgroup/memory/docker/x/memory.limit_in_bytes",
                "/sys/fs/cgroup/memory/docker/memory.limit_in_bytes",
                "/sys/fs/cgroup/memory/memory.limit_in_bytes",
            ]
        );
        assert!(cgroup_limit_files("garbage\n").is_empty());
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
