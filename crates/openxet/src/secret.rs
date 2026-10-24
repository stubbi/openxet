//! Generated secrets and the files that hold them.
//!
//! Tokens openxet generates are `oxt_` followed by 43 base64url characters,
//! the unpadded encoding of 32 bytes from the operating system's random
//! number generator (ruling M1-R28). The fixed prefix lets secret scanners
//! find a token committed by accident.
//!
//! Secret files are mode `0600` (the umask can only narrow it), never
//! overwritten, and refused when other users may read them. Each is written
//! whole to a temporary file, synced, then linked to its name, which fails
//! if the name exists: a crash never leaves a truncated secret, and of two
//! processes creating the same file at once, one wins and the other reads
//! the winner's.

use std::fs::{self, File, OpenOptions};
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

fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// Whether `text` has the form of a generated token.
pub fn is_token(text: &str) -> bool {
    text.len() == TOKEN_LEN
        && text.starts_with(TOKEN_PREFIX)
        && text[TOKEN_PREFIX.len()..].bytes().all(is_token_char)
}

/// Whether `text` holds something of a generated token's form, as the
/// repository's secret scan matches it: `oxt_` and 43 token characters.
pub fn contains_token(text: &str) -> bool {
    text.match_indices(TOKEN_PREFIX).any(|(at, _)| {
        text.as_bytes()[at + TOKEN_PREFIX.len()..]
            .iter()
            .take_while(|b| is_token_char(**b))
            .count()
            >= TOKEN_LEN - TOKEN_PREFIX.len()
    })
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

/// Creates `dir` and its missing parents, each mode `0700` on unix.
pub fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Creates the new file `path` with `mode` on unix.
pub fn create_new_file(path: &Path, mode: u32) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode;
    options.open(path)
}

fn exists_error(path: &Path) -> CliError {
    CliError::new(
        "exists",
        format!("{} already exists", path.display()),
        "Nothing was overwritten. Remove the file first, or pass another path.",
    )
}

/// Writes `contents` to a new file at `path` with `mode`, atomically.
///
/// # Errors
///
/// `exists` if the file already exists: nothing is ever overwritten.
pub fn create_with_mode(path: &Path, contents: &[u8], mode: u32) -> Result<(), CliError> {
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty());
    if let Some(parent) = parent {
        create_private_dir_all(parent).map_err(|err| {
            io_error(
                "create the directory",
                parent,
                &err,
                "Check the path and its permissions.",
            )
        })?;
    }
    let name = path
        .file_name()
        .map_or_else(|| "secret".into(), |n| n.to_string_lossy().into_owned());
    let suffix = hex(&random_bytes()?[..8]);
    let tmp = path.with_file_name(format!(".{name}.{suffix}.tmp"));
    let written = create_new_file(&tmp, mode).and_then(|mut file| {
        file.write_all(contents)?;
        file.sync_all()
    });
    if let Err(err) = written {
        let _ = fs::remove_file(&tmp);
        return Err(io_error(
            "write",
            &tmp,
            &err,
            "Check free space and permissions.",
        ));
    }
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    match linked {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(exists_error(path)),
        // A filesystem without hard links: fall back to an exclusive,
        // non-atomic write.
        Err(_) => {
            let mut file = create_new_file(path, mode).map_err(|err| {
                if err.kind() == io::ErrorKind::AlreadyExists {
                    exists_error(path)
                } else {
                    io_error("create", path, &err, "Check the path and its permissions.")
                }
            })?;
            file.write_all(contents)
                .and_then(|()| file.sync_all())
                .map_err(|err| {
                    io_error("write", path, &err, "Check free space and permissions.")
                })?;
        }
    }
    // The new name survives a power loss only once its directory is synced.
    if let Some(dir) = parent.and_then(|p| File::open(p).ok()) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// [`create_with_mode`] with mode `0600`.
pub fn create(path: &Path, contents: &[u8]) -> Result<(), CliError> {
    create_with_mode(path, contents, 0o600)
}

/// The contents of the file at `path`, or `None` if it doesn't exist,
/// without checking its permissions.
pub fn read_unchecked(path: &Path) -> Result<Option<Zeroizing<String>>, CliError> {
    match fs::read_to_string(path) {
        Ok(contents) => Ok(Some(Zeroizing::new(contents))),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(io_error(
            "read",
            path,
            &err,
            "Check that the file is readable by the user running openxet.",
        )),
    }
}

/// Checks that no group or other user may access the file at `path`.
///
/// # Errors
///
/// `insecure_permissions`, with the `chmod` that fixes it.
pub fn check_private(path: &Path) -> Result<(), CliError> {
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
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The contents of the secret file at `path`, or `None` if it doesn't
/// exist.
///
/// # Errors
///
/// `insecure_permissions` if group or other users may access the file.
pub fn read(path: &Path) -> Result<Option<Zeroizing<String>>, CliError> {
    let Some(contents) = read_unchecked(path)? else {
        return Ok(None);
    };
    check_private(path)?;
    Ok(Some(contents))
}

/// The secret at `path` parsed by `parse`, creating it from `generate`
/// first if the file doesn't exist. Returns it and whether this call
/// created the file. If another process creates it at the same moment,
/// that process's secret is read and used.
fn load_or_create<T>(
    path: &Path,
    parse: impl Fn(&str) -> Result<T, CliError>,
    generate: impl FnOnce() -> Result<Zeroizing<String>, CliError>,
) -> Result<(T, bool), CliError> {
    if let Some(text) = read(path)? {
        return Ok((parse(&text)?, false));
    }
    let text = generate()?;
    match create(path, text.as_bytes()) {
        Ok(()) => Ok((parse(&text)?, true)),
        Err(err) if err.code == "exists" => {
            let text = read(path)?.ok_or(err)?;
            Ok((parse(&text)?, false))
        }
        Err(err) => Err(err),
    }
}

/// The 32-byte transfer key at `path`, or `None` if the file doesn't
/// exist. It is 64 hex characters.
pub fn read_transfer_key(path: &Path) -> Result<Option<Zeroizing<[u8; 32]>>, CliError> {
    read(path)?
        .map(|text| parse_transfer_key(path, &text))
        .transpose()
}

fn parse_transfer_key(path: &Path, text: &str) -> Result<Zeroizing<[u8; 32]>, CliError> {
    parse_key(text.trim()).ok_or_else(|| {
        CliError::new(
            "invalid_key",
            format!("{} is not a transfer key", path.display()),
            "A transfer key is 64 hex characters (`openssl rand -hex 32`). Restore the file, \
             or remove it to generate a new key, which invalidates outstanding transfer URLs.",
        )
    })
}

/// The transfer key at `path`, creating a random one if the file doesn't
/// exist, and whether this call created it. The key signs transfer URLs,
/// so every node behind one endpoint must share it.
pub fn transfer_key(path: &Path) -> Result<(Zeroizing<[u8; 32]>, bool), CliError> {
    load_or_create(
        path,
        |text| parse_transfer_key(path, text),
        || {
            Ok(Zeroizing::new(format!(
                "{}\n",
                hex(random_bytes()?.as_ref())
            )))
        },
    )
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

fn parse_dev_token(path: &Path, text: &str) -> Result<Zeroizing<String>, CliError> {
    let token = Zeroizing::new(text.trim().to_owned());
    if is_token(&token) {
        Ok(token)
    } else {
        Err(CliError::new(
            "invalid_key",
            format!("{} does not hold an openxet token", path.display()),
            "Remove the file to generate a new dev token; clients must then use the new one.",
        ))
    }
}

/// Checks the dev-token file at `path`, if there is one.
pub fn check_dev_token(path: &Path) -> Result<(), CliError> {
    read(path)?.map_or(Ok(()), |text| parse_dev_token(path, &text).map(drop))
}

/// The dev-mode token at `path`, creating one if the file doesn't exist.
pub fn dev_token(path: &Path) -> Result<Zeroizing<String>, CliError> {
    load_or_create(
        path,
        |text| parse_dev_token(path, text),
        || Ok(Zeroizing::new(format!("{}\n", generate_token()?.as_str()))),
    )
    .map(|(token, _)| token)
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
        assert!(contains_token(&format!("sha256 = \"{}\"", a.as_str())));
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
        assert!(!contains_token("oxt_short and oxt_"));
    }

    #[test]
    fn secret_files_are_private_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/secret");
        create(&path, b"one").unwrap();
        assert_eq!(create(&path, b"two").unwrap_err().code, "exists");
        assert_eq!(read(&path).unwrap().unwrap().as_str(), "one");
        // No temporary file is left behind.
        assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(read(&path).unwrap_err().code, "insecure_permissions");
        }
    }

    #[test]
    fn keys_and_tokens_are_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transfer.key");
        let (key, created) = transfer_key(&path).unwrap();
        assert!(created);
        assert_eq!(transfer_key(&path).unwrap(), (key, false));
        let token = dev_token(&dir.path().join("dev-token")).unwrap();
        assert_eq!(dev_token(&dir.path().join("dev-token")).unwrap(), token);
    }

    #[test]
    fn concurrent_creators_agree_on_one_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dev-token");
        let tokens: Vec<String> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| s.spawn(|| dev_token(&path).unwrap().as_str().to_owned()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(tokens.iter().all(|t| *t == tokens[0]), "{tokens:?}");
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
