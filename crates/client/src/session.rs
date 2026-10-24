//! What every transfer shares: its options, the client core's session
//! configuration, the cache directory and the progress ticker.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use http::HeaderMap;
use http::header::{HeaderValue, USER_AGENT};
use xet_client::cas_client::auth::AuthConfig;
use xet_data::processing::configurations::{SessionContext, TranslatorConfig};
use xet_data::progress_tracking::GroupProgressReport;
use xet_runtime::config::XetConfig;
use xet_runtime::core::{XetContext, xet_cache_root};

use crate::{ClientError, Endpoint};

/// How often a progress callback is called.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

const USER_AGENT_VALUE: &str = concat!("openxet/", env!("CARGO_PKG_VERSION"));

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
    /// For a download: replace the output if it exists.
    pub overwrite: bool,
}

/// The client core's configuration for one session: telemetry off
/// (non-negotiable 6), `User-Agent: openxet/{version}`, and a fresh
/// session id, as `data_client::default_config` builds it but with the
/// cache root set.
pub(crate) fn translator(ep: &Endpoint, options: &Options) -> xet_data::Result<TranslatorConfig> {
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

/// Creates the cache root, mode `0700`, if it doesn't exist: its shards
/// list the user's file and chunk hashes. An existing root is left as it
/// is, since `hf_xet` shares it.
pub(crate) fn prepare_cache(options: &Options) -> Result<(), ClientError> {
    let root = options.cache_dir.clone().unwrap_or_else(xet_cache_root);
    if root.is_dir() {
        return Ok(());
    }
    create_private_dir_all(&root).map_err(|err| ClientError::io(&root, err))
}

fn create_private_dir_all(dir: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Calls the progress callback every [`PROGRESS_INTERVAL`] until dropped.
pub(crate) struct Ticker(Option<tokio::task::JoinHandle<()>>);

impl Ticker {
    pub(crate) fn start(
        options: &Options,
        report: impl Fn() -> GroupProgressReport + Send + 'static,
    ) -> Self {
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
