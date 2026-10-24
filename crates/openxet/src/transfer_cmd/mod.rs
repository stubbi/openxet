//! `openxet upload` and `openxet download`: thin wrappers over
//! [`openxet_client`], which carries the protocol (ADR 0006) and lands
//! downloads atomically.
//!
//! The endpoint and token come from `--endpoint` and `--token`, else
//! `OPENXET_ENDPOINT` and `OPENXET_TOKEN`.

mod failure;
mod report;
mod run;

use std::ffi::OsString;
use std::io::{self, IsTerminal as _};
use std::path::PathBuf;
use std::sync::Arc;

use openxet_client::{ClientError, Endpoint, Options};

use crate::error::CliError;
use crate::output::Output;
use failure::{client_error, usage_error};
use report::{DownloadResult, UploadResult};
use run::ProgressLine;

const ENDPOINT_VAR: &str = "OPENXET_ENDPOINT";
const TOKEN_VAR: &str = "OPENXET_TOKEN";

/// Where to transfer, with which token and cache.
#[derive(Debug, clap::Args)]
pub struct RemoteArgs {
    /// The CAS endpoint, as `openxet serve` prints it [default: `$OPENXET_ENDPOINT`]
    #[arg(long, value_name = "URL")]
    endpoint: Option<String>,
    /// The bearer token [default: `$OPENXET_TOKEN`]. Prefer the variable: a
    /// command-line argument is visible to other users of this machine
    #[arg(long, value_name = "TOKEN")]
    token: Option<String>,
    /// The Xet cache directory, in place of `HF_XET_CACHE` [default: as
    /// `hf_xet`: `$HF_XET_CACHE`, `$HF_HOME/xet`, or `~/.cache/huggingface/xet`]
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
}

/// `openxet upload`'s options.
#[derive(Debug, clap::Args)]
pub struct UploadArgs {
    /// Files to upload, taken literally (no globbing or directory walking)
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,
    #[command(flatten)]
    remote: RemoteArgs,
}

/// `openxet download`'s options.
#[derive(Debug, clap::Args)]
pub struct DownloadArgs {
    /// The file's Xet hash, as `openxet upload` prints it
    #[arg(value_name = "HASH")]
    hash: String,
    /// Where to write the file
    #[arg(long, short, value_name = "PATH")]
    output: PathBuf,
    /// The file's size in bytes; a file of another size is an error
    #[arg(long, value_name = "N")]
    size: Option<u64>,
    /// Replace PATH if it exists
    #[arg(long)]
    force: bool,
    #[command(flatten)]
    remote: RemoteArgs,
}

pub fn upload(args: &UploadArgs, output: Output) -> Result<(), CliError> {
    let (endpoint, options, progress) = resolve(&args.remote, output, "Uploading", false)?;
    let uploads = run::block_on(
        openxet_client::upload(&endpoint, &args.paths, &options),
        progress.as_deref(),
    )?
    .map_err(|err| run::failure(&err))?;
    output
        .emit(&UploadResult::from(uploads))
        .map_err(|err| CliError::output(&err))
}

pub fn download(args: &DownloadArgs, output: Output) -> Result<(), CliError> {
    if !openxet_client::is_hash(&args.hash) {
        return Err(client_error(&ClientError::InvalidHash));
    }
    let (endpoint, options, progress) = resolve(&args.remote, output, "Downloading", args.force)?;
    let size = run::block_on(
        openxet_client::download(&endpoint, &args.hash, args.size, &args.output, &options),
        progress.as_deref(),
    )?
    .map_err(|err| run::failure(&err))?;
    output
        .emit(&DownloadResult {
            schema: "openxet.download.v1",
            hash: args.hash.clone(),
            path: args.output.to_string_lossy().into_owned(),
            size,
        })
        .map_err(|err| CliError::output(&err))
}

/// The endpoint, token and options from the flags and the environment,
/// and the progress line if stderr is a terminal a person reads.
fn resolve(
    remote: &RemoteArgs,
    output: Output,
    verb: &'static str,
    overwrite: bool,
) -> Result<(Endpoint, Options, Option<Arc<ProgressLine>>), CliError> {
    let url = flag_or_env(remote.endpoint.as_ref(), ENDPOINT_VAR).ok_or_else(|| {
        usage_error(
            "missing_endpoint",
            "no endpoint: neither --endpoint nor OPENXET_ENDPOINT is set",
            "Set OPENXET_ENDPOINT to the endpoint `openxet serve` prints, for example \
             `export OPENXET_ENDPOINT=http://127.0.0.1:8080`.",
        )
    })?;
    let token = flag_or_env(remote.token.as_ref(), TOKEN_VAR).ok_or_else(|| {
        usage_error(
            "missing_token",
            "no token: neither --token nor OPENXET_TOKEN is set",
            "Set OPENXET_TOKEN to a token for this server: a dev server prints one on startup \
             (and keeps it in <data dir>/dev-token); otherwise ask the operator or run \
             `openxet token mint`.",
        )
    })?;
    let endpoint = Endpoint::new(&url, token).map_err(|err| client_error(&err))?;
    let progress = (output == Output::Human && io::stderr().is_terminal())
        .then(|| Arc::new(ProgressLine::new(verb)));
    let on_progress = progress.clone().map(|line| {
        let on_progress: openxet_client::OnProgress = Arc::new(move |p| line.draw(p));
        on_progress
    });
    Ok((
        endpoint,
        Options {
            cache_dir: remote.cache_dir.clone(),
            on_progress,
            overwrite,
        },
        progress,
    ))
}

/// The flag's value, else the variable's; an empty value counts as unset.
fn flag_or_env(flag: Option<&String>, var: &str) -> Option<String> {
    flag.cloned()
        .or_else(|| std::env::var_os(var).map(OsString::into_string)?.ok())
        .filter(|v| !v.is_empty())
}
