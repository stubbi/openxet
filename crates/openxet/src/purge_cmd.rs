//! `openxet purge`: unregisters a file from every scope (ruling M2-R13),
//! working on the server's own data directory, with or without the server
//! running.
//!
//! The bucket's purge record is written first, then the index rows go,
//! then the audit row: the bucket is the source of truth (design §5.3), so
//! a purge interrupted after its record is finished by running it again,
//! and `reindex` (M4) skips the file either way. Nothing is deleted from
//! the bucket (non-negotiable 3): the file's xorbs stay until garbage
//! collection finds them unreferenced.

use std::io::{self, BufRead as _, IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use openxet_client::ClientError;
use openxet_index::{AuditEntry, IndexError, Repos as _, SqliteIndex};
use openxet_protocol::{MerkleHash, hash_from_hex};
use openxet_storage::{Bucket as _, LocalBucket, PurgeRecord, StorageError};
use serde::Serialize;
use serde_json::json;

use crate::config::{self, Overrides};
use crate::error::CliError;
use crate::output::{HumanDisplay, Output};
use crate::serve::{BUCKET_DIR, INDEX_FILE};
use crate::transfer_cmd::{client_error, usage_error};

/// `openxet purge`'s options.
#[derive(Debug, clap::Args)]
pub struct PurgeArgs {
    /// The file's Xet hash, as `openxet upload` printed it
    #[arg(value_name = "HASH")]
    hash: String,
    /// The server's TOML configuration file, for its data directory
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// The server's data directory, in place of `server.data_dir`
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Report how many scopes the file would be removed from, and change
    /// nothing
    #[arg(long)]
    dry_run: bool,
    /// Purge without asking; required with `--json` and without a terminal
    #[arg(long)]
    yes: bool,
    /// Who is purging, as the audit log and the purge record name it
    /// [default: `cli:` and the operating system user]
    #[arg(long, value_name = "NAME")]
    actor: Option<String>,
}

/// The `openxet.purge.v1` result.
#[derive(Serialize)]
struct Purged {
    schema: &'static str,
    file: String,
    /// The scopes the file was removed from, or with `dry_run` would be.
    scopes_removed: u64,
    dry_run: bool,
}

impl HumanDisplay for Purged {
    fn human(&self) -> String {
        let scopes = scopes(self.scopes_removed);
        if self.dry_run {
            format!(
                "Dry run: purging {} would remove it from {scopes}. Nothing was changed.",
                self.file
            )
        } else {
            format!(
                "Purged {}: removed it from {scopes}. No scope serves it now, and the purge \
                 record in the bucket keeps a reindex from restoring it.",
                self.file
            )
        }
    }
}

fn scopes(n: u64) -> String {
    if n == 1 {
        "1 scope".to_owned()
    } else {
        format!("{n} scopes")
    }
}

pub fn run(args: &PurgeArgs, output: Output) -> Result<(), CliError> {
    if !openxet_client::is_hash(&args.hash) {
        return Err(client_error(&ClientError::InvalidHash));
    }
    let file = hash_from_hex(&args.hash).map_err(|_| client_error(&ClientError::InvalidHash))?;
    if !args.yes && !args.dry_run && output == Output::Json {
        return Err(confirmation_required(
            "`openxet purge --json` never asks for confirmation",
        ));
    }
    let overrides = Overrides {
        data_dir: args.data_dir.clone(),
        ..Overrides::default()
    };
    let loaded = config::load(args.config.as_deref(), std::env::vars_os(), &overrides);
    let config = loaded
        .config
        .ok_or_else(|| CliError::config(&loaded.problems))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| {
            CliError::new(
                "runtime",
                format!("could not start the async runtime: {err}"),
                "Check the process's thread and file-descriptor limits.",
            )
        })?;
    let purged = runtime.block_on(purge(&config.data_dir, file, args))?;
    output.emit(&purged).map_err(|err| CliError::output(&err))
}

async fn purge(data_dir: &Path, file: MerkleHash, args: &PurgeArgs) -> Result<Purged, CliError> {
    let index = open_index(data_dir).await?;
    let present = index
        .file_scope_count(&file)
        .await
        .map_err(|err| index_error(&err))?;
    let result = |scopes_removed, dry_run| Purged {
        schema: "openxet.purge.v1",
        file: args.hash.clone(),
        scopes_removed,
        dry_run,
    };
    if args.dry_run {
        return Ok(result(present, true));
    }
    // Only a purge that writes names who ran it.
    let actor = actor(args.actor.as_deref())?;
    if !args.yes {
        confirm(&args.hash, present)?;
    }
    let bucket = open_bucket(data_dir)?;
    let record = PurgeRecord {
        file,
        actor: actor.clone(),
        at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    // Bucket first (ruling M2-R13). A record that already exists is a
    // purge run before, perhaps interrupted: its index rows go now.
    bucket
        .create(&record.key(), record.to_bytes())
        .await
        .map_err(|err| storage_error(data_dir, &err))?;
    let removed = index
        .purge_file(&file)
        .await
        .map_err(|err| index_error(&err))?;
    index
        .audit(&AuditEntry {
            actor,
            action: "purge".to_owned(),
            repo: None,
            detail: json!({ "file": args.hash, "scopes_removed": removed }),
        })
        .await
        .map_err(|err| index_error(&err))?;
    Ok(result(removed, false))
}

/// `--actor`, or `cli:` and the operating system user.
fn actor(flag: Option<&str>) -> Result<String, CliError> {
    let actor = if let Some(actor) = flag {
        actor.to_owned()
    } else {
        let user = ["USER", "LOGNAME", "USERNAME"]
            .into_iter()
            .find_map(|var| std::env::var(var).ok().filter(|v| !v.is_empty()))
            .ok_or_else(|| {
                usage_error(
                    "no_actor",
                    "could not tell who is purging: none of USER, LOGNAME and USERNAME is set",
                    "Pass `--actor NAME`; the audit log and the purge record name it.",
                )
            })?;
        format!("cli:{user}")
    };
    if actor.is_empty() || actor.chars().any(char::is_control) {
        return Err(usage_error(
            "invalid_actor",
            "the actor is empty or holds a control character",
            "Pass `--actor NAME` with a printable name, such as your user name.",
        ));
    }
    Ok(actor)
}

fn confirmation_required(why: &str) -> CliError {
    usage_error(
        "confirmation_required",
        &format!("{why}; nothing was purged"),
        "Pass `--yes` to purge, or `--dry-run` to see how many scopes it would remove the \
         file from.",
    )
}

/// Asks on the terminal; a run without one must pass `--yes`.
fn confirm(hash: &str, present: u64) -> Result<(), CliError> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(confirmation_required(
            "there is no terminal to confirm the purge on",
        ));
    }
    eprint!(
        "Purge {hash} from {}? No scope will serve it again. Type `yes` to purge: ",
        scopes(present)
    );
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin().lock().read_line(&mut answer).map_err(|err| {
        CliError::new(
            "io",
            format!("could not read the answer: {err}"),
            "Pass `--yes` to purge without asking.",
        )
    })?;
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(CliError::new(
            "declined",
            "the purge was not confirmed; nothing was purged",
            "Type `yes` to purge, or pass `--yes`.",
        ))
    }
}

const DATA_DIR_HINT: &str = "Pass the server's `--config`, or `--data-dir` with the directory \
     `openxet serve` uses (it prints it on startup).";

/// The server's index. A data directory without one was never served
/// from: it is refused rather than given a new, empty index.
async fn open_index(dir: &Path) -> Result<SqliteIndex, CliError> {
    let path = dir.join(INDEX_FILE);
    if !path.is_file() {
        return Err(CliError::new(
            "index",
            format!("{} holds no openxet index ({INDEX_FILE})", dir.display()),
            DATA_DIR_HINT,
        ));
    }
    SqliteIndex::open(&path).await.map_err(|err| {
        CliError::new(
            "index",
            format!("could not open the index in {}: {err}", dir.display()),
            "Check that you may write the data directory, as the server's user does.",
        )
    })
}

fn open_bucket(dir: &Path) -> Result<LocalBucket, CliError> {
    let path = dir.join(BUCKET_DIR);
    if !path.is_dir() {
        return Err(CliError::new(
            "storage",
            format!("{} holds no openxet bucket ({BUCKET_DIR}/)", dir.display()),
            DATA_DIR_HINT,
        ));
    }
    LocalBucket::open(&path).map_err(|err| storage_error(dir, &err))
}

fn storage_error(dir: &Path, err: &StorageError) -> CliError {
    CliError::new(
        "storage",
        format!(
            "could not write to the bucket in {}: {err}; nothing was purged",
            dir.display()
        ),
        "Check that you may write the data directory, as the server's user does, and that its \
         disk has space; then run the purge again.",
    )
}

fn index_error(err: &IndexError) -> CliError {
    CliError::new(
        "index",
        format!("the index failed: {err}"),
        "Check that you may write the data directory, as the server's user does, then run the \
         purge again: repeating it is safe, and finishes a purge whose bucket record was \
         already written.",
    )
}
