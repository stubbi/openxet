//! `openxet upload` and `openxet download`: thin wrappers over
//! [`openxet_client`], which carries the protocol (ADR 0006).
//!
//! The endpoint and token come from `--endpoint` and `--token`, else
//! `OPENXET_ENDPOINT` and `OPENXET_TOKEN`. Downloads land atomically:
//! they are written to `PATH.openxet-partial`, synced, then renamed.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use openxet_client::{ClientError, Endpoint, Options, Progress, Uploaded};
use serde::Serialize;

use crate::error::{CliError, ExitCode, FileError};
use crate::output::{HumanDisplay, Output};

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

/// The `openxet.upload.v1` result.
#[derive(Serialize)]
struct UploadResult {
    schema: &'static str,
    files: Vec<UploadedFile>,
}

#[derive(Serialize)]
struct UploadedFile {
    path: String,
    hash: String,
    size: u64,
    sha256: String,
}

impl From<Uploaded> for UploadedFile {
    fn from(u: Uploaded) -> Self {
        Self {
            path: u.path.to_string_lossy().into_owned(),
            hash: u.hash,
            size: u.size,
            sha256: u.sha256,
        }
    }
}

impl HumanDisplay for UploadResult {
    fn human(&self) -> String {
        let width = self
            .files
            .iter()
            .map(|f| f.path.chars().count())
            .chain(["PATH".len()])
            .max()
            .unwrap_or(0);
        let size_width = self
            .files
            .iter()
            .map(|f| f.size.to_string().len())
            .chain(["SIZE".len()])
            .max()
            .unwrap_or(0);
        let mut lines = vec![format!(
            "{:<width$}  {:<64}  {:>size_width$}  SHA256",
            "PATH", "XET HASH", "SIZE"
        )];
        lines.extend(self.files.iter().map(|f| {
            format!(
                "{:<width$}  {}  {:>size_width$}  {}",
                f.path, f.hash, f.size, f.sha256
            )
        }));
        lines.join("\n")
    }
}

/// The `openxet.download.v1` result.
#[derive(Serialize)]
struct DownloadResult {
    schema: &'static str,
    hash: String,
    path: String,
    size: u64,
}

impl HumanDisplay for DownloadResult {
    fn human(&self) -> String {
        format!(
            "Downloaded {} to {} ({} bytes).",
            self.hash, self.path, self.size
        )
    }
}

pub fn upload(args: &UploadArgs, output: Output) -> Result<(), CliError> {
    let (endpoint, options, progress) = resolve(&args.remote, output, "Uploading")?;
    let uploaded = block_on(
        openxet_client::upload(&endpoint, &args.paths, &options),
        progress.as_deref(),
    )?
    .map_err(|err| client_error(&err))?;
    output
        .emit(&UploadResult {
            schema: "openxet.upload.v1",
            files: uploaded.into_iter().map(UploadedFile::from).collect(),
        })
        .map_err(|err| CliError::output(&err))
}

pub fn download(args: &DownloadArgs, output: Output) -> Result<(), CliError> {
    if !openxet_client::is_hash(&args.hash) {
        return Err(client_error(&ClientError::InvalidHash));
    }
    if !args.force && fs::symlink_metadata(&args.output).is_ok() {
        return Err(output_exists(&args.output));
    }
    let (endpoint, options, progress) = resolve(&args.remote, output, "Downloading")?;
    let partial = partial_path(&args.output);
    let fetched = block_on(
        openxet_client::download(&endpoint, &args.hash, args.size, &partial, &options),
        progress.as_deref(),
    )?;
    let size = fetched
        .map_err(|err| client_error(&err))
        .and_then(|size| land(&partial, &args.output, args.force).map(|()| size))
        .inspect_err(|_| {
            let _ = fs::remove_file(&partial);
        })?;
    output
        .emit(&DownloadResult {
            schema: "openxet.download.v1",
            hash: args.hash.clone(),
            path: args.output.to_string_lossy().into_owned(),
            size,
        })
        .map_err(|err| CliError::output(&err))
}

/// `PATH.openxet-partial`.
fn partial_path(out: &Path) -> PathBuf {
    let mut name = out.as_os_str().to_owned();
    name.push(".openxet-partial");
    PathBuf::from(name)
}

/// Syncs `partial` and moves it to `out`, so `out` is either absent or
/// complete. Without `force`, `out` is created with a hard link, which
/// fails if it exists, so a file that appeared meanwhile is never
/// replaced; where hard links aren't supported, a rename after a final
/// check does the same with a short race.
fn land(partial: &Path, out: &Path, force: bool) -> Result<(), CliError> {
    let io_at = |path: &Path| {
        let path = path.to_owned();
        move |source| client_error(&ClientError::Io { path, source })
    };
    File::open(partial)
        .and_then(|f| f.sync_all())
        .map_err(io_at(partial))?;
    if force {
        fs::rename(partial, out).map_err(io_at(out))?;
    } else {
        match fs::hard_link(partial, out) {
            Ok(()) => fs::remove_file(partial).map_err(io_at(partial))?,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                return Err(output_exists(out));
            }
            Err(_) => {
                if fs::symlink_metadata(out).is_ok() {
                    return Err(output_exists(out));
                }
                fs::rename(partial, out).map_err(io_at(out))?;
            }
        }
    }
    sync_parent(out).map_err(io_at(out))
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

fn output_exists(path: &Path) -> CliError {
    usage_error(
        "output_exists",
        &format!("{} exists; nothing was downloaded", path.display()),
        "Pass `--force` to replace it, or choose another `--output` path.",
    )
}

/// The endpoint, token and options from the flags and the environment,
/// and the progress line if stderr is a terminal a person reads.
fn resolve(
    remote: &RemoteArgs,
    output: Output,
    verb: &'static str,
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
    let progress = (output == Output::Human && io::stderr().is_terminal()).then(|| {
        Arc::new(ProgressLine {
            verb,
            drawn: AtomicBool::new(false),
        })
    });
    let on_progress = progress.clone().map(|line| {
        let on_progress: openxet_client::OnProgress = Arc::new(move |p| line.draw(p));
        on_progress
    });
    Ok((
        endpoint,
        Options {
            cache_dir: remote.cache_dir.clone(),
            on_progress,
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

/// A progress line on stderr, redrawn in place.
struct ProgressLine {
    verb: &'static str,
    drawn: AtomicBool,
}

impl ProgressLine {
    fn draw(&self, Progress { done, total }: Progress) {
        // Nothing is known yet: an empty line would only flicker.
        if total == 0 {
            return;
        }
        let mut stderr = io::stderr().lock();
        let _ = write!(
            stderr,
            "\r{} {} / {} ({}%)\x1b[K",
            self.verb,
            human_bytes(done),
            human_bytes(total),
            done.min(total) * 100 / total
        );
        let _ = stderr.flush();
        self.drawn.store(true, Ordering::Relaxed);
    }

    /// Ends the line, so what follows starts on its own.
    fn finish(&self) {
        if self.drawn.swap(false, Ordering::Relaxed) {
            let _ = writeln!(io::stderr());
        }
    }
}

#[allow(clippy::cast_precision_loss)] // display only
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Runs `fut` on a multi-thread runtime, which the client core needs to
/// share rather than start its own pool (S10 "Context"), then ends the
/// progress line.
fn block_on<F: Future>(fut: F, progress: Option<&ProgressLine>) -> Result<F::Output, CliError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            CliError::new(
                "runtime",
                format!("could not start the async runtime: {err}"),
                "Check the process's thread and file-descriptor limits.",
            )
        })?;
    let output = runtime.block_on(fut);
    if let Some(progress) = progress {
        progress.finish();
    }
    Ok(output)
}

fn usage_error(code: &'static str, message: &str, hint: &'static str) -> CliError {
    CliError::new(code, message, hint).with_exit(ExitCode::USAGE)
}

/// The stable code, message and hint for a client failure. The message is
/// the client's, which never holds the token (`ClientError`'s invariant).
fn client_error(err: &ClientError) -> CliError {
    let message = err.to_string();
    let (code, hint): (&'static str, &'static str) = match err {
        ClientError::InvalidEndpoint { .. } => {
            return usage_error(
                "invalid_endpoint",
                &message,
                "Set OPENXET_ENDPOINT (or --endpoint) to the endpoint `openxet serve` prints, \
                 such as http://127.0.0.1:8080.",
            );
        }
        ClientError::InvalidHash => {
            return usage_error(
                "invalid_hash",
                &message,
                "Pass the Xet hash `openxet upload` printed for the file: 64 characters 0-9 \
                 and a-f.",
            );
        }
        ClientError::InvalidInput { problems } => {
            let files = problems
                .iter()
                .map(|p| FileError {
                    path: p.path.to_string_lossy().into_owned(),
                    problem: p.problem.code(),
                })
                .collect();
            return usage_error(
                "invalid_input",
                &format!("{message}; nothing was uploaded"),
                "Pass paths of existing, readable regular files; directories are not uploaded.",
            )
            .with_files(files);
        }
        ClientError::TokenExpired { .. } => (
            "token_expired",
            "Get a new token, for example with `openxet token mint`, and set OPENXET_TOKEN.",
        ),
        ClientError::Unreachable { .. } => (
            "unreachable_endpoint",
            "Check that the server is running and that OPENXET_ENDPOINT (or --endpoint) is the \
             endpoint it printed on startup.",
        ),
        ClientError::Unauthorized { .. } => (
            "unauthorized",
            "Check that OPENXET_TOKEN is a token for this server, not revoked or expired. A dev \
             server prints its token on startup and keeps it in <data dir>/dev-token.",
        ),
        ClientError::Forbidden { .. } => (
            "forbidden",
            "The token is valid but doesn't allow this: uploading needs a write token for the \
             scope. Ask the operator for one, or mint one with `openxet token mint --mode \
             write`.",
        ),
        ClientError::NotFound { .. } => (
            "not_found",
            "Check the hash `openxet upload` printed, and that the token's scope holds the file.",
        ),
        ClientError::UploadRejected { .. } => (
            "upload_rejected",
            "The message is the server's reason; follow it. Nothing from this upload was \
             registered.",
        ),
        ClientError::Io { .. } => (
            "io",
            "Check that the path exists and that you may read it (upload) or write its \
             directory (download).",
        ),
        ClientError::Status { .. } => (
            "server_error",
            "Retry later. If it keeps failing, give the server's operator the time of this run: \
             the server log records the cause.",
        ),
        ClientError::Transfer { .. } => (
            "transfer_failed",
            "Retry. If it keeps failing, report it with the command you ran.",
        ),
    };
    CliError::new(code, message, hint)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_upload_table_aligns_its_columns() {
        let hash = "a".repeat(64);
        let sha = "b".repeat(64);
        let table = UploadResult {
            schema: "openxet.upload.v1",
            files: vec![
                UploadedFile {
                    path: "a.bin".into(),
                    hash: hash.clone(),
                    size: 7,
                    sha256: sha.clone(),
                },
                UploadedFile {
                    path: "models/large.bin".into(),
                    hash: hash.clone(),
                    size: 3_145_728,
                    sha256: sha.clone(),
                },
            ],
        }
        .human();
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines.len(), 3, "{table}");
        assert!(lines[0].starts_with("PATH  "), "{table}");
        let column = |needle: &str| lines.iter().map(|l| l.find(needle)).collect::<Vec<_>>();
        let hashes = column(&hash);
        assert_eq!(hashes[1], hashes[2], "{table}");
        assert_eq!(lines[0].find("XET HASH"), hashes[1], "{table}");
        assert!(lines[1].contains("        7  "), "{table}");
    }

    #[test]
    fn the_partial_file_sits_next_to_the_output() {
        assert_eq!(
            partial_path(Path::new("dir/model.bin")),
            Path::new("dir/model.bin.openxet-partial")
        );
    }

    #[test]
    fn sizes_are_shown_in_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(3 << 20), "3.0 MiB");
    }

    #[test]
    fn landing_without_force_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let (partial, out) = (dir.path().join("p"), dir.path().join("o"));
        std::fs::write(&partial, b"new").unwrap();
        std::fs::write(&out, b"old").unwrap();
        let err = land(&partial, &out, false).unwrap_err();
        assert_eq!(err.code, "output_exists");
        assert_eq!(std::fs::read(&out).unwrap(), b"old");

        land(&partial, &out, true).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), b"new");
        assert!(!partial.exists());

        std::fs::write(&partial, b"newer").unwrap();
        let fresh = dir.path().join("fresh");
        land(&partial, &fresh, false).unwrap();
        assert_eq!(std::fs::read(&fresh).unwrap(), b"newer");
        assert!(!partial.exists());
    }

    #[test]
    fn client_errors_never_carry_the_token_and_always_a_hint() {
        let token = "oxt_secret-marker";
        let endpoint = || "http://127.0.0.1:1".to_owned();
        let errors = [
            Endpoint::new("http://u:oxt_secret-marker@h", token.into()).unwrap_err(),
            ClientError::TokenExpired { exp: 1 },
            ClientError::Unreachable {
                endpoint: endpoint(),
                reason: "the connection was refused".into(),
            },
            ClientError::Unauthorized {
                endpoint: endpoint(),
            },
            ClientError::Forbidden {
                endpoint: endpoint(),
            },
            ClientError::NotFound {
                hash: "ab".repeat(32),
            },
            ClientError::UploadRejected {
                message: "xorb … is not stored on this server".into(),
            },
            ClientError::InvalidHash,
            ClientError::Status { status: 503 },
            ClientError::Transfer { kind: "internal" },
        ];
        for err in errors {
            let cli = client_error(&err);
            assert!(!cli.message.contains("secret-marker"), "{cli:?}");
            assert!(!cli.hint.is_empty(), "{cli:?}");
        }
    }
}
