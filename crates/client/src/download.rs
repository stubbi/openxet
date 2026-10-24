//! Download over `xet-data` 1.7.0, landing the file atomically.
//!
//! The bytes go to a new file with an unpredictable name next to the
//! output, created with `O_CREAT | O_EXCL` (which never follows a symlink
//! planted at the name), and the client core writes only to that open
//! handle (`download_to_writer`), never to a path it opens itself. Once
//! complete and synced, the file is renamed (or, without `overwrite`,
//! linked) into place. On any failure, or if the future is dropped, the
//! partial file is removed.

use std::fs::{self, File, Permissions};
use std::io;
use std::path::Path;
use std::sync::Arc;

use tempfile::NamedTempFile;
use xet_data::processing::{FileDownloadSession, XetFileInfo};

use crate::error::{Context, Operation};
use crate::session::{Options, Progress, Ticker, prepare_cache, translator};
use crate::{ClientError, Endpoint, net};

/// The partial file's name ends in this, after the output's name and a
/// random part: `.{name}.{random}.openxet-partial`.
pub const PARTIAL_SUFFIX: &str = ".openxet-partial";

/// Downloads the file with Xet hash `hash` to `out` and returns its size.
/// With `size`, a file of any other size is [`ClientError::SizeMismatch`]
/// and `out` is left alone.
///
/// # Errors
///
/// A [`ClientError`] naming the cause. `out` is then unchanged, and no
/// partial file is left.
pub async fn download(
    ep: &Endpoint,
    hash: &str,
    size: Option<u64>,
    out: &Path,
    options: &Options,
) -> Result<u64, ClientError> {
    if !is_hash(hash) {
        return Err(ClientError::InvalidHash);
    }
    if !options.overwrite && fs::symlink_metadata(out).is_ok() {
        return Err(ClientError::OutputExists {
            path: out.to_owned(),
        });
    }
    let proxy = net::proxy_for(ep);
    net::preflight(ep, proxy.as_ref()).await?;
    let partial = partial_file(out)?;
    let shown_proxy = proxy.as_ref().map(net::shown);
    let cx = Context {
        endpoint: &ep.url,
        op: Operation::Download,
        proxy: shown_proxy.as_deref(),
        hash: Some(hash),
        path: Some(partial.path()),
        token_exp: ep.token_exp,
    };
    let writer = partial
        .as_file()
        .try_clone()
        .map_err(|err| ClientError::io(partial.path(), err))?;
    prepare_cache(options)?;
    let translator = Arc::new(translator(ep, options).map_err(|e| ClientError::from_data(e, &cx))?);
    let session = FileDownloadSession::new(translator, None)
        .await
        .map_err(|e| ClientError::from_data(e, &cx))?;
    let ticker = {
        let session = Arc::clone(&session);
        Ticker::start(options, move || session.report())
    };
    // Always by hash alone: a wrong expected size trips xet-data 1.7.0's
    // progress debug assertions (`progress_types.rs`), so the size is
    // compared here instead.
    let info = XetFileInfo::new_hash_only(hash.to_owned());
    let result = async {
        let (_id, n_bytes) = session.download_to_writer(&info, .., writer).await?;
        session.finalize().await?;
        Ok(n_bytes)
    }
    .await;
    drop(ticker);
    let n_bytes = result.map_err(|e| ClientError::from_data(e, &cx))?;
    if let Some(expected) = size
        && expected != n_bytes
    {
        return Err(ClientError::SizeMismatch {
            expected,
            actual: n_bytes,
        });
    }
    if let Some(on_progress) = &options.on_progress {
        on_progress(Progress {
            done: n_bytes,
            total: n_bytes,
        });
    }
    land(partial, out, options.overwrite)?;
    Ok(n_bytes)
}

/// A new, empty file next to `out`, with the mode a plain create would
/// give it.
fn partial_file(out: &Path) -> Result<NamedTempFile, ClientError> {
    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = out.file_name().ok_or_else(|| {
        ClientError::io(
            out,
            io::Error::new(io::ErrorKind::InvalidInput, "the output names no file"),
        )
    })?;
    let mut builder = tempfile::Builder::new();
    let prefix = format!(".{}.", name.to_string_lossy());
    builder.prefix(&prefix).suffix(PARTIAL_SUFFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        // Before the umask, as for any new file.
        builder.permissions(Permissions::from_mode(0o666));
    }
    builder
        .tempfile_in(dir)
        .map_err(|err| ClientError::io(dir, err))
}

/// Syncs `partial` and moves it to `out`. Without `overwrite` it is
/// linked into place, which fails if `out` exists, so a file that appeared
/// meanwhile is never replaced.
fn land(partial: NamedTempFile, out: &Path, overwrite: bool) -> Result<(), ClientError> {
    partial
        .as_file()
        .sync_all()
        .map_err(|err| ClientError::io(partial.path(), err))?;
    let landed = if overwrite {
        partial.persist(out)
    } else {
        partial.persist_noclobber(out)
    };
    landed.map_err(|err| {
        if err.error.kind() == io::ErrorKind::AlreadyExists && !overwrite {
            ClientError::OutputExists {
                path: out.to_owned(),
            }
        } else {
            ClientError::io(out, err.error)
        }
    })?;
    sync_parent(out).map_err(|err| ClientError::io(out, err))
}

/// Makes the rename durable: on Unix, a directory entry survives a crash
/// only once the directory is synced.
fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// 64 lowercase hex characters: a Xet hash as the CAS routes name it.
#[must_use]
pub fn is_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn partial_with(dir: &Path, out: &Path, data: &[u8]) -> NamedTempFile {
        let partial = partial_file(out).unwrap();
        assert_eq!(partial.path().parent().unwrap(), dir);
        std::fs::write(partial.path(), data).unwrap();
        partial
    }

    #[test]
    fn landing_without_overwrite_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("o");
        std::fs::write(&out, b"old").unwrap();
        let partial = partial_with(dir.path(), &out, b"new");
        let path = partial.path().to_owned();
        let err = land(partial, &out, false).unwrap_err();
        assert!(matches!(err, ClientError::OutputExists { .. }), "{err:?}");
        assert_eq!(std::fs::read(&out).unwrap(), b"old");
        assert!(!path.exists(), "the partial file was left");

        land(partial_with(dir.path(), &out, b"new"), &out, true).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"new");

        let fresh = dir.path().join("fresh");
        land(partial_with(dir.path(), &fresh, b"newer"), &fresh, false).unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"newer");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn partial_names_are_unpredictable_and_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("model.bin");
        let (a, b) = (partial_file(&out).unwrap(), partial_file(&out).unwrap());
        assert_ne!(a.path(), b.path());
        let name = a.path().file_name().unwrap().to_str().unwrap().to_owned();
        assert!(name.starts_with(".model.bin."), "{name}");
        assert!(name.ends_with(PARTIAL_SUFFIX), "{name}");
    }
}
