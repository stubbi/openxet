//! Upload over `xet-data` 1.7.0, as S10 "Our client core" sketches it.

use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use xet_data::processing::{FileUploadSession, Sha256Policy};

use crate::error::{Context, InputProblem, Operation, Problem};
use crate::session::{Options, Progress, Ticker, prepare_cache, translator};
use crate::{ClientError, Endpoint, net};

/// One uploaded file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uploaded {
    /// The path as given.
    pub path: PathBuf,
    /// The Xet file hash, 64 lowercase hex characters.
    pub hash: String,
    pub size: u64,
    /// The SHA-256 of the contents, 64 lowercase hex characters.
    pub sha256: String,
}

/// What an upload did: each file, and how much of their contents had to
/// be sent rather than deduplicated against the server and the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Uploads {
    pub files: Vec<Uploaded>,
    /// Content bytes in chunks the server didn't have, before compression.
    pub bytes_uploaded: u64,
    /// Content bytes in chunks the server or the client cache already had.
    pub bytes_deduplicated: u64,
}

/// Uploads `paths`, deduplicating against the client cache and the
/// server's global dedup. Nothing is registered until every file is
/// chunked and every xorb is stored: one shard upload registers them all.
///
/// # Errors
///
/// A [`ClientError`] naming the cause. Paths that are missing, not
/// regular files or unreadable are all reported, as
/// [`ClientError::InvalidInput`], before anything is sent.
pub async fn upload(
    ep: &Endpoint,
    paths: &[PathBuf],
    options: &Options,
) -> Result<Uploads, ClientError> {
    let sizes = input_sizes(paths)?;
    let proxy = net::proxy_for(ep);
    net::preflight(ep, proxy.as_ref()).await?;
    let shown_proxy = proxy.as_ref().map(net::shown);
    let cx = |op, path| Context {
        endpoint: &ep.url,
        op,
        proxy: shown_proxy.as_deref(),
        hash: None,
        path,
        token_exp: ep.token_exp,
    };
    prepare_cache(options)?;
    let translator = Arc::new(
        translator(ep, options)
            .map_err(|e| ClientError::from_data(e, &cx(Operation::Upload, None)))?,
    );
    let session = FileUploadSession::new(translator)
        .await
        .map_err(|e| ClientError::from_data(e, &cx(Operation::Upload, None)))?;
    let ticker = {
        let session = Arc::clone(&session);
        Ticker::start(options, move || session.report())
    };
    // One task per file, so a local failure names its file.
    let mut tasks = Vec::with_capacity(paths.len());
    for path in paths {
        let (_id, task) = session
            .spawn_upload_from_path(path.clone(), Sha256Policy::Compute)
            .await
            .map_err(|e| ClientError::from_data(e, &cx(Operation::Upload, Some(path))))?;
        tasks.push(task);
    }
    let mut infos = Vec::with_capacity(paths.len());
    for (path, task) in paths.iter().zip(tasks) {
        let (info, _) = task
            .await
            .map_err(|_| ClientError::Transfer {
                kind: "a file's upload task ended abnormally",
            })?
            .map_err(|e| ClientError::from_data(e, &cx(Operation::Upload, Some(path))))?;
        infos.push(info);
    }
    let metrics = Arc::clone(&session)
        .finalize()
        .await
        .map_err(|e| ClientError::from_data(e, &cx(Operation::Finalize, None)))?;
    drop(ticker);
    if let Some(on_progress) = &options.on_progress {
        let total = sizes.iter().sum();
        on_progress(Progress { done: total, total });
    }
    let files = paths
        .iter()
        .zip(infos)
        .zip(sizes)
        .map(|((path, info), size)| {
            let size = info.file_size().unwrap_or(size);
            let sha256 = info.sha256().ok_or(ClientError::Transfer {
                kind: "the client core computed no SHA-256",
            })?;
            Ok(Uploaded {
                path: path.clone(),
                hash: info.hash().to_owned(),
                size,
                sha256: reported_sha256(sha256, size).to_owned(),
            })
        })
        .collect::<Result<_, ClientError>>()?;
    Ok(Uploads {
        files,
        bytes_uploaded: metrics.new_bytes,
        bytes_deduplicated: metrics.deduped_bytes,
    })
}

/// The SHA-256 of zero bytes.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// The contents' SHA-256 as the user can check it. For an empty file
/// xet-data 1.7.0 reports, and writes into the shard, all zeros, which the
/// protocol accepts (S6); the user is shown the real digest instead.
fn reported_sha256(core: &str, size: u64) -> &str {
    if size == 0 && core.bytes().all(|b| b == b'0') {
        EMPTY_SHA256
    } else {
        core
    }
}

/// Each path's size, or every path that is not a readable regular file.
/// Each file is opened once, to prove it can be read.
fn input_sizes(paths: &[PathBuf]) -> Result<Vec<u64>, ClientError> {
    let mut sizes = Vec::with_capacity(paths.len());
    let mut problems = Vec::new();
    for path in paths {
        let checked = File::open(path).and_then(|file| file.metadata());
        let problem = match checked {
            Ok(meta) if meta.is_file() => {
                sizes.push(meta.len());
                continue;
            }
            Ok(_) => Problem::NotAFile,
            // `open` succeeds on a directory on Unix but not on Windows.
            Err(_) if path.is_dir() => Problem::NotAFile,
            Err(err) if err.kind() == io::ErrorKind::NotFound => Problem::NotFound,
            Err(err) => Problem::Unreadable(err.kind()),
        };
        problems.push(InputProblem {
            path: path.clone(),
            problem,
        });
    }
    if problems.is_empty() {
        Ok(sizes)
    } else {
        Err(ClientError::InvalidInput { problems })
    }
}
