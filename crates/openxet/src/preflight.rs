//! Checks of the files a valid configuration names, for `openxet config
//! check` and server startup: the data directory and the secret files.
//!
//! The checks only read, except that the data directory's writability is
//! tested by creating and removing one empty file in it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{Config, Problem};
use crate::error::CliError;
use crate::secret;

/// What the files a configuration names would do to `serve`.
#[derive(Debug, Default)]
pub struct Findings {
    /// What would make `serve` fail.
    pub problems: Vec<Problem>,
    /// What `serve` accepts but warns about.
    pub warnings: Vec<Problem>,
}

/// The warning for a data directory other users may enter, if `dir` is
/// one. The fix is [`shared_data_dir_fix`].
pub fn shared_data_dir(dir: &Path) -> Option<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(dir).ok()?.permissions().mode();
        (mode & 0o077 != 0).then(|| {
            format!(
                "the data directory {} is accessible to other users (mode {:o}), who can read \
                 every stored file",
                dir.display(),
                mode & 0o777,
            )
        })
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        None
    }
}

/// What fixes [`shared_data_dir`].
pub fn shared_data_dir_fix(dir: &Path) -> String {
    format!("Run `chmod 700 {}`.", dir.display())
}

/// The error for a `dir` that is a symbolic link to nothing: it can be
/// neither used nor created.
pub fn dangling_symlink(dir: &Path) -> Option<CliError> {
    let is_link = fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link || dir.exists() {
        return None;
    }
    let target = fs::read_link(dir).map_or_else(|_| "?".to_owned(), |t| t.display().to_string());
    Some(CliError::new(
        "data_dir",
        format!(
            "the data directory {} is a symbolic link to {target}, which does not exist",
            dir.display()
        ),
        format!(
            "Create {target} (`mkdir -m 700 {target}`), or pass `--data-dir` with another \
             directory."
        ),
    ))
}

/// The error for a `cas.transfer_key_file` that was named but is missing.
pub fn missing_transfer_key(path: &Path) -> CliError {
    CliError::new(
        "no_key",
        format!("the transfer key file {} does not exist", path.display()),
        format!(
            "Create it once and copy it to every node: `(umask 077; openssl rand -hex 32 > {})`. \
             Or remove cas.transfer_key_file to keep the key in the data directory, where \
             openxet creates it.",
            path.display()
        ),
    )
}

/// The checks of `config`'s files.
pub fn check(config: &Config) -> Findings {
    let mut findings = Findings::default();
    let dir = &config.data_dir;
    let problem = |key: &str, value: &Path, err: CliError| Problem {
        key: key.to_owned(),
        value: Some(value.display().to_string()),
        source: "the file system".to_owned(),
        rule: err.message,
        hint: err.hint.into_owned(),
        quoted: true,
    };

    if let Err(err) = writable(dir) {
        findings.problems.push(problem("server.data_dir", dir, err));
    } else if let Some(warning) = shared_data_dir(dir) {
        findings.warnings.push(Problem {
            key: "server.data_dir".to_owned(),
            value: Some(dir.display().to_string()),
            source: "the file system".to_owned(),
            rule: warning,
            hint: shared_data_dir_fix(dir),
            quoted: true,
        });
    }

    let key = &config.transfer_key_file;
    match secret::read_transfer_key(key) {
        Ok(None) if config.transfer_key_explicit => findings.problems.push(problem(
            "cas.transfer_key_file",
            key,
            missing_transfer_key(key),
        )),
        Ok(_) => {}
        Err(err) => findings
            .problems
            .push(problem("cas.transfer_key_file", key, err)),
    }

    if config.auth.is_none() {
        let token = dir.join("dev-token");
        if let Err(err) = secret::check_dev_token(&token) {
            findings
                .problems
                .push(problem("server.data_dir", &token, err));
        }
    }
    findings
}

/// Checks that `dir`, or the nearest existing directory above it, accepts
/// new files.
fn writable(dir: &Path) -> Result<(), CliError> {
    if let Some(err) = dangling_symlink(dir) {
        return Err(err);
    }
    let mut existing: PathBuf = dir.to_owned();
    while !existing.exists() {
        match existing.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => existing = parent.to_owned(),
            _ => {
                existing = PathBuf::from(".");
                break;
            }
        }
    }
    let unwritable = |err: io::Error| {
        CliError::new(
            "data_dir",
            format!(
                "the data directory {} can't be written: {} does not accept new files ({err})",
                dir.display(),
                existing.display()
            ),
            "Pass `--data-dir` with a directory the user running openxet may write, or set \
             server.data_dir.",
        )
    };
    if !existing.is_dir() {
        return Err(unwritable(io::Error::other("it is not a directory")));
    }
    let suffix = secret::hex(&secret::random_bytes()?[..8]);
    let probe = existing.join(format!(".openxet-check-{suffix}"));
    secret::create_new_file(&probe, 0o600).map_err(unwritable)?;
    let _ = fs::remove_file(&probe);
    Ok(())
}
