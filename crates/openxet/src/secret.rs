//! Generated secrets and the files that hold them.
//!
//! Tokens openxet generates are `oxt_` followed by 43 base64url characters,
//! the unpadded encoding of 32 bytes from the operating system's random
//! number generator (ruling M1-R28). The fixed prefix lets secret scanners
//! find a token committed by accident.
//!
//! Secret files are created with mode `0600`, never overwritten, and refused
//! when other users may read them.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use zeroize::Zeroizing;

use crate::error::CliError;

/// The prefix of every token openxet generates.
pub const TOKEN_PREFIX: &str = "oxt_";

/// Length of a generated token: the prefix and 43 base64url characters.
pub const TOKEN_LEN: usize = TOKEN_PREFIX.len() + 43;

/// 32 bytes from the operating system's random number generator.
pub fn random_bytes() -> Result<Zeroizing<[u8; 32]>, CliError> {
    let mut bytes = Zeroizing::new([0u8; 32]);
    getrandom::fill(bytes.as_mut()).map_err(|_| {
        CliError::new(
            "random",
            "the operating system's random number generator failed",
            "Retry; if it persists, check the host's entropy source.",
        )
    })?;
    Ok(bytes)
}

/// A new token: `oxt_` and 32 random bytes in unpadded base64url.
pub fn generate_token() -> Result<Zeroizing<String>, CliError> {
    let bytes = random_bytes()?;
    Ok(Zeroizing::new(format!(
        "{TOKEN_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(bytes.as_ref())
    )))
}

/// Whether `text` has the form of a generated token.
pub fn is_token(text: &str) -> bool {
    text.len() == TOKEN_LEN
        && text.starts_with(TOKEN_PREFIX)
        && text[TOKEN_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn io_error(what: &str, path: &Path, err: &io::Error, hint: &'static str) -> CliError {
    CliError::new(
        "io",
        format!("could not {what} {}: {err}", path.display()),
        hint,
    )
}

/// Writes `contents` to a new file at `path` with mode `0600`.
///
/// # Errors
///
/// `exists` if the file already exists: a secret is never overwritten.
pub fn create(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|err| {
            io_error(
                "create the directory",
                parent,
                &err,
                "Check the path and its permissions.",
            )
        })?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|err| {
        if err.kind() == io::ErrorKind::AlreadyExists {
            CliError::new(
                "exists",
                format!("{} already exists", path.display()),
                "Nothing was overwritten. Remove the file first, or pass another path.",
            )
        } else {
            io_error("create", path, &err, "Check the path and its permissions.")
        }
    })?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|err| io_error("write", path, &err, "Check free space and permissions."))
}

/// The contents of the secret file at `path`, or `None` if it doesn't
/// exist.
///
/// # Errors
///
/// `insecure_permissions` if group or other users may access the file.
pub fn read(path: &Path) -> Result<Option<Zeroizing<String>>, CliError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => Zeroizing::new(contents),
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(io_error(
                "read",
                path,
                &err,
                "Check that the file is readable by the user running openxet.",
            ));
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path)
            .map_err(|err| io_error("inspect", path, &err, "Check the file's permissions."))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(CliError::new(
                "insecure_permissions",
                format!(
                    "{} holds a secret but other users may access it (mode {:o})",
                    path.display(),
                    mode & 0o777
                ),
                format!("Run `chmod 600 {}`, then start again.", path.display()),
            ));
        }
    }
    Ok(Some(contents))
}

/// The 32-byte transfer key at `path`, written as 64 hex characters,
/// creating a random one if the file doesn't exist. The key signs transfer
/// URLs, so every node behind one endpoint must share it.
pub fn transfer_key(path: &Path) -> Result<Zeroizing<[u8; 32]>, CliError> {
    if let Some(text) = read(path)? {
        return parse_key(text.trim()).ok_or_else(|| {
            CliError::new(
                "invalid_key",
                format!("{} is not a transfer key", path.display()),
                "A transfer key is 64 hex characters (`openssl rand -hex 32`). Restore the \
                 file, or remove it to generate a new key, which invalidates outstanding transfer \
                 URLs.",
            )
        });
    }
    let key = random_bytes()?;
    let text = Zeroizing::new(format!("{}\n", hex(key.as_ref())));
    create(path, text.as_bytes())?;
    Ok(key)
}

fn parse_key(text: &str) -> Option<Zeroizing<[u8; 32]>> {
    if text.len() != 64 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut key = Zeroizing::new([0u8; 32]);
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(key)
}

/// The dev-mode token at `path`, creating one if the file doesn't exist.
pub fn dev_token(path: &Path) -> Result<Zeroizing<String>, CliError> {
    if let Some(text) = read(path)? {
        let token = Zeroizing::new(text.trim().to_owned());
        if is_token(&token) {
            return Ok(token);
        }
        return Err(CliError::new(
            "invalid_key",
            format!("{} does not hold an openxet token", path.display()),
            "Remove the file to generate a new dev token; clients must then use the new one.",
        ));
    }
    let token = generate_token()?;
    create(path, format!("{}\n", token.as_str()).as_bytes())?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn generated_tokens_have_the_scannable_form() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert!(is_token(&a), "{}", a.as_str());
        assert_eq!(a.len(), 47);
        assert_ne!(a, b);
        let decoded = URL_SAFE_NO_PAD.decode(&a[TOKEN_PREFIX.len()..]).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn token_form_is_exact() {
        assert!(!is_token("oxt_short"));
        assert!(!is_token(&format!("xyz_{}", "a".repeat(43))));
        assert!(!is_token(&format!("oxt_{}=", "a".repeat(42))));
        assert!(is_token(&format!(
            "oxt_{}",
            "-_aZ9".repeat(9)[..43].to_owned()
        )));
    }

    #[test]
    fn secret_files_are_private_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/secret");
        create(&path, b"one").unwrap();
        assert_eq!(create(&path, b"two").unwrap_err().code, "exists");
        assert_eq!(read(&path).unwrap().unwrap().as_str(), "one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(read(&path).unwrap_err().code, "insecure_permissions");
        }
    }

    #[test]
    fn keys_and_tokens_are_reused() {
        let dir = tempfile::tempdir().unwrap();
        let key = transfer_key(&dir.path().join("transfer.key")).unwrap();
        assert_eq!(transfer_key(&dir.path().join("transfer.key")).unwrap(), key);
        let token = dev_token(&dir.path().join("dev-token")).unwrap();
        assert_eq!(dev_token(&dir.path().join("dev-token")).unwrap(), token);
    }

    #[test]
    fn a_corrupt_key_is_refused_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer.key");
        create(&path, b"not hex\n").unwrap();
        assert_eq!(transfer_key(&path).unwrap_err().code, "invalid_key");
        assert_eq!(fs::read(&path).unwrap(), b"not hex\n");
    }
}
