//! Upload and download over `xet-data` 1.7.0, as S10 "Our client core"
//! sketches it.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use http::header::{HeaderValue, USER_AGENT};
use xet_client::cas_client::auth::AuthConfig;
use xet_data::processing::configurations::{SessionContext, TranslatorConfig};
use xet_data::processing::{FileDownloadSession, FileUploadSession, Sha256Policy, XetFileInfo};
use xet_data::progress_tracking::GroupProgressReport;
use xet_runtime::config::XetConfig;
use xet_runtime::core::XetContext;

use crate::error::{Context, InputProblem, Problem};
use crate::{ClientError, Endpoint};

/// How long the connection check waits for the endpoint to accept.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How often a progress callback is called.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

/// Bytes processed so far out of the bytes known so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

/// A progress callback.
pub type OnProgress = Arc<dyn Fn(Progress) + Send + Sync>;

/// How a transfer runs, beyond where it goes.
#[derive(Clone, Default)]
pub struct Options {
    /// The Xet cache root, used as `HF_XET_CACHE` would be. Without it the
    /// client core picks the root as it does for `hf_xet`: `HF_XET_CACHE`,
    /// `$HF_HOME/xet`, `$XDG_CACHE_HOME/huggingface/xet`, then
    /// `~/.cache/huggingface/xet` (S10 "Cache dirs").
    pub cache_dir: Option<PathBuf>,
    /// Called while bytes are processed, and once when they all are.
    pub on_progress: Option<OnProgress>,
}

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
) -> Result<Vec<Uploaded>, ClientError> {
    let sizes = input_sizes(paths)?;
    preflight(ep).await?;
    let cx = Context {
        endpoint: &ep.url,
        hash: None,
        path: paths
            .first()
            .map(PathBuf::as_path)
            .filter(|_| paths.len() == 1),
        token_exp: ep.token_exp,
    };
    let translator = Arc::new(translator(ep, options).map_err(|e| ClientError::from_data(e, &cx))?);
    let session = FileUploadSession::new(translator)
        .await
        .map_err(|e| ClientError::from_data(e, &cx))?;
    let ticker = {
        let session = Arc::clone(&session);
        Ticker::start(options, move || session.report())
    };
    let result = async {
        let infos = session
            .upload_files(paths.iter().map(|p| (p.as_path(), Sha256Policy::Compute)))
            .await?;
        Arc::clone(&session).finalize().await?;
        Ok(infos)
    }
    .await;
    drop(ticker);
    let infos = result.map_err(|e| ClientError::from_data(e, &cx))?;
    if let Some(on_progress) = &options.on_progress {
        let total = sizes.iter().sum();
        on_progress(Progress { done: total, total });
    }
    paths
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
        .collect()
}

/// Downloads the file with Xet hash `hash` to `out`, which the caller has
/// made sure may be written, and returns its size. With `size`, a file of
/// any other size is an error.
///
/// # Errors
///
/// A [`ClientError`] naming the cause. `out` may hold partial contents;
/// the caller writes to a temporary path and renames it.
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
    preflight(ep).await?;
    let cx = Context {
        endpoint: &ep.url,
        hash: Some(hash),
        path: Some(out),
        token_exp: ep.token_exp,
    };
    let translator = Arc::new(translator(ep, options).map_err(|e| ClientError::from_data(e, &cx))?);
    let session = FileDownloadSession::new(translator, None)
        .await
        .map_err(|e| ClientError::from_data(e, &cx))?;
    let info = match size {
        Some(size) => XetFileInfo::new(hash.to_owned(), size),
        None => XetFileInfo::new_hash_only(hash.to_owned()),
    };
    let ticker = {
        let session = Arc::clone(&session);
        Ticker::start(options, move || session.report())
    };
    let result = async {
        let (_id, n_bytes) = session.download_file(&info, out).await?;
        session.finalize().await?;
        Ok(n_bytes)
    }
    .await;
    drop(ticker);
    let n_bytes = result.map_err(|e| ClientError::from_data(e, &cx))?;
    if let Some(on_progress) = &options.on_progress {
        on_progress(Progress {
            done: n_bytes,
            total: n_bytes,
        });
    }
    Ok(n_bytes)
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

/// The client core's configuration for one session: telemetry off
/// (non-negotiable 6), `User-Agent: openxet/{version}`, and a fresh
/// session id, as `data_client::default_config` builds it but with the
/// cache root set.
fn translator(ep: &Endpoint, options: &Options) -> xet_data::Result<TranslatorConfig> {
    let mut config = XetConfig::new();
    // After the environment is read, so HF_XET_TELEMETRY_ENABLED can't
    // turn it back on.
    config.telemetry.enabled = false;
    let ctx = XetContext::with_config(config)?;
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    let session = SessionContext {
        endpoint: ep.url.clone(),
        auth: AuthConfig::maybe_new(Some(ep.token.clone()), Some(ep.token_exp), None),
        custom_headers: Some(Arc::new(headers)),
        repo_paths: vec![String::new()],
        session_id: Some(uuid::Uuid::now_v7().to_string()),
        cache_root: options.cache_dir.clone(),
    };
    TranslatorConfig::new(&ctx, session)
}

const USER_AGENT_VALUE: &str = concat!("openxet/", env!("CARGO_PKG_VERSION"));

/// Fails fast, before the client core's retries (about two minutes by
/// default), when the token has expired or nothing listens at the
/// endpoint. Behind an HTTP proxy the endpoint is not dialled directly,
/// so the connection check is skipped.
async fn preflight(ep: &Endpoint) -> Result<(), ClientError> {
    ep.check_expiry()?;
    if proxy_configured() {
        return Ok(());
    }
    let Some((host, port)) = ep.authority() else {
        return Ok(());
    };
    let unreachable = |reason: String| ClientError::Unreachable {
        endpoint: ep.url.clone(),
        reason,
    };
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(unreachable(connect_error(&err))),
        Err(_) => Err(unreachable(format!(
            "no answer within {}s",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

fn connect_error(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::ConnectionRefused => "the connection was refused".to_owned(),
        io::ErrorKind::TimedOut => "the connection timed out".to_owned(),
        _ => err.to_string(),
    }
}

/// Whether reqwest would send requests through a proxy (`HTTP_PROXY`,
/// `HTTPS_PROXY` or `ALL_PROXY`, in either case).
fn proxy_configured() -> bool {
    ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"]
        .iter()
        .any(|name| {
            [name.to_string(), name.to_ascii_lowercase()]
                .iter()
                .any(|n| std::env::var_os(n).is_some_and(|v| !v.is_empty()))
        })
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

/// Calls the progress callback every [`PROGRESS_INTERVAL`] until dropped.
struct Ticker(Option<tokio::task::JoinHandle<()>>);

impl Ticker {
    fn start(options: &Options, report: impl Fn() -> GroupProgressReport + Send + 'static) -> Self {
        Self(options.on_progress.clone().map(|on_progress| {
            tokio::spawn(async move {
                loop {
                    let report = report();
                    on_progress(Progress {
                        done: report.total_bytes_completed,
                        total: report.total_bytes,
                    });
                    tokio::time::sleep(PROGRESS_INTERVAL).await;
                }
            })
        }))
    }
}

impl Drop for Ticker {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// 64 lowercase hex characters: a Xet hash as the CAS routes name it.
#[must_use]
pub fn is_hash(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
