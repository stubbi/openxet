//! `openxet proc-receive`: the `proc-receive` hook of every push openxet
//! serves (ADR 0004 items 3 and 7). git runs it through the symlink
//! `{git.cache_dir}/hooks/proc-receive`, which names this executable, so it
//! also runs when started under that name, with no arguments. It is hidden
//! from `--help`: it only works with the channel the server hands down.

use std::ffi::OsStr;
use std::path::Path;

/// The hook's name, as git invokes it.
const HOOK_NAME: &str = "proc-receive";

/// Whether this process was started as the hook, through the symlink.
pub fn invoked_as_hook(argv0: Option<&OsStr>) -> bool {
    argv0
        .and_then(|a| Path::new(a).file_name())
        .is_some_and(|name| name == HOOK_NAME)
}

/// Runs the hook and returns its exit code.
pub fn run() -> u8 {
    openxet_git::hook::proc_receive_hook()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::invoked_as_hook;

    #[test]
    fn hook_name_is_the_file_name() {
        assert!(invoked_as_hook(Some(OsStr::new(
            "/var/lib/openxet/git-cache/hooks/proc-receive"
        ))));
        assert!(invoked_as_hook(Some(OsStr::new("proc-receive"))));
        assert!(!invoked_as_hook(Some(OsStr::new("/usr/bin/openxet"))));
        assert!(!invoked_as_hook(Some(OsStr::new(
            "/hooks/proc-receive/openxet"
        ))));
        assert!(!invoked_as_hook(None));
    }
}
