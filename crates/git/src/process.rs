//! The git process: finding git, its version floor (ADR 0004 item 8), the
//! sanitised environment every git child gets (item 7) and the config every
//! snapshot pins (item 1).

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::{GitError, SnapshotRepo};

/// The oldest git the Git role runs on: cruft packs need 2.37, MIDX bitmaps
/// 2.34 and `proc-receive` 2.29 (ADR 0004 item 8).
pub const MIN_GIT_VERSION: (u32, u32, u32) = (2, 37, 0);

/// The config every snapshot repository pins (ADR 0004 item 1), beside
/// `core.hooksPath`, which names openxet's hook directory under the cache.
///
/// `receive.autogc=false` goes beyond the ADR's list: a snapshot is thrown
/// away after its request, so the `git gc --auto` receive-pack would start
/// after a push could only spend CPU on a copy nobody reads; repacking is
/// the leaseholder's job (ADR 0004 item 5).
pub const PINNED_CONFIG: &[(&str, &str)] = &[
    ("uploadpack.allowAnySHA1InWant", "false"),
    ("uploadpack.allowReachableSHA1InWant", "true"),
    ("uploadpack.allowRefInWant", "false"),
    ("uploadpack.allowFilter", "false"),
    ("core.alternateRefsCommand", "true"),
    ("receive.procReceiveRefs", "refs/"),
    ("receive.fsckObjects", "true"),
    ("receive.unpackLimit", "1"),
    ("transfer.advertiseObjectInfo", "false"),
    ("uploadpack.advertiseBundleURIs", "false"),
    ("promisor.advertise", "false"),
    ("transfer.advertiseSID", "false"),
    ("receive.autogc", "false"),
];

/// What every git child's environment is built from.
#[derive(Debug)]
pub(crate) struct ProcessEnv {
    path: OsString,
    home: PathBuf,
}

impl ProcessEnv {
    /// The environment for asking git about itself, before its exec dir is
    /// known: an empty `PATH`, as git is run by its absolute path.
    pub(crate) fn bootstrap(home: &Path) -> Result<Self, GitError> {
        Self::with_dirs(&[], home)
    }

    /// The environment of every git child: `PATH` is exactly the exec dir
    /// and the hook dir (ADR 0004 item 7). git starts its helpers from the
    /// exec dir and shell commands with `/bin/sh` by absolute path, so it
    /// needs no system directory there.
    pub(crate) fn new(exec_dir: &Path, hooks: &Path, home: &Path) -> Result<Self, GitError> {
        Self::with_dirs(&[exec_dir, hooks], home)
    }

    fn with_dirs(dirs: &[&Path], home: &Path) -> Result<Self, GitError> {
        let path = std::env::join_paths(dirs).map_err(|_| {
            let bad = dirs
                .iter()
                .find(|d| d.as_os_str().as_encoded_bytes().contains(&b':'))
                .map_or_else(PathBuf::new, |d| d.to_path_buf());
            GitError::Io {
                op: "put on git's PATH",
                path: bad,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "the directory name contains ':'; move git.cache_dir or git to a path without one",
                ),
            }
        })?;
        Ok(Self {
            path,
            home: home.to_path_buf(),
        })
    }

    /// A `git` command with only the ADR 0004 item 7 variables set.
    pub(crate) fn command(&self, git: &Path) -> Command {
        let mut cmd = Command::new(git);
        cmd.env_clear()
            .env("PATH", &self.path)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .kill_on_drop(true);
        cmd
    }
}

/// A `git` command for `snapshot`, run in its directory with the sanitised
/// environment of ADR 0004 item 7: `PATH` (the git exec dir and openxet's
/// hook dir), `HOME` (an empty directory of the cache),
/// `GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL=/dev/null`, and
/// `GIT_PROTOCOL=version={protocol}` only for protocol 0, 1 or 2. Nothing
/// of openxet's own environment reaches git. The caller adds the arguments,
/// and receive-pack's caller its hook variables. The child is killed if the
/// command's handle is dropped, so an abandoned request leaves no git
/// behind.
#[must_use]
pub fn git_command(git: &Path, snapshot: &SnapshotRepo, protocol: Option<u8>) -> Command {
    let mut cmd = snapshot.process.command(git);
    if let Some(version) = protocol.filter(|v| *v <= 2) {
        cmd.env("GIT_PROTOCOL", format!("version={version}"));
    }
    cmd.current_dir(snapshot.dir.path());
    cmd
}

/// The protocol version a client's `Git-Protocol` header (or SSH
/// `GIT_PROTOCOL` variable) asks for, if it is exactly `version=0`,
/// `version=1` or `version=2`; anything else is ignored, which serves the
/// client protocol v0, as git does for a value it doesn't know.
#[must_use]
pub fn protocol_version(value: &[u8]) -> Option<u8> {
    match value {
        b"version=0" => Some(0),
        b"version=1" => Some(1),
        b"version=2" => Some(2),
        _ => None,
    }
}

/// The first `git` on openxet's `PATH`. Relative entries are skipped, so the
/// working directory never decides which git runs.
pub(crate) fn find_git() -> Result<PathBuf, GitError> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("git"))
        .find(|git| {
            std::fs::metadata(git).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .ok_or(GitError::GitNotFound)
}

/// Runs `git {args}` and returns its trimmed stdout.
async fn ask(
    git: &Path,
    env: &ProcessEnv,
    args: &'static [&'static str],
) -> Result<String, GitError> {
    let out = env
        .command(git)
        .args(args)
        .output()
        .await
        .map_err(|source| GitError::Io {
            op: "run",
            path: git.to_path_buf(),
            source,
        })?;
    if !out.status.success() {
        return Err(GitError::GitFailed {
            git: git.to_path_buf(),
            command: args.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Checks `git --version` against [`MIN_GIT_VERSION`].
pub(crate) async fn check_version(git: &Path, env: &ProcessEnv) -> Result<(), GitError> {
    let found = ask(git, env, &["--version"]).await?;
    match parse_version(&found) {
        Some(version) if version >= MIN_GIT_VERSION => Ok(()),
        _ => Err(GitError::GitVersion {
            path: git.to_path_buf(),
            found,
        }),
    }
}

/// `git --exec-path`: where git's helper programs live.
pub(crate) async fn exec_path(git: &Path, env: &ProcessEnv) -> Result<PathBuf, GitError> {
    ask(git, env, &["--exec-path"]).await.map(PathBuf::from)
}

/// The version in `git version 2.39.5 (Apple Git-154)` and the like; a
/// missing patch level reads as 0.
fn parse_version(output: &str) -> Option<(u32, u32, u32)> {
    let version = output.strip_prefix("git version ")?.split(' ').next()?;
    let mut parts = version.split('.').map(|p| {
        let digits = p.bytes().take_while(u8::is_ascii_digit).count();
        p[..digits].parse::<u32>().ok()
    });
    let major = parts.next()??;
    let minor = parts.next()??;
    let patch = parts.next().flatten().unwrap_or(0);
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn versions_parse() {
        assert_eq!(
            parse_version("git version 2.39.5 (Apple Git-154)"),
            Some((2, 39, 5))
        );
        assert_eq!(parse_version("git version 2.37.0"), Some((2, 37, 0)));
        assert_eq!(
            parse_version("git version 2.45.windows.1"),
            Some((2, 45, 0))
        );
        assert_eq!(parse_version("git version 2.50.0.rc1"), Some((2, 50, 0)));
        assert_eq!(parse_version("git version 3.0"), Some((3, 0, 0)));
        assert_eq!(parse_version("not git"), None);
        assert_eq!(parse_version("git version x.y"), None);
    }
}
