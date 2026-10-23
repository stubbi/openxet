use std::io;

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::output::{HumanDisplay, Output};

/// Process exit codes, documented in `docs/reference/exit-codes.md`.
pub struct ExitCode;

impl ExitCode {
    pub const SUCCESS: u8 = 0;
    pub const FAILURE: u8 = 1;
    pub const USAGE: u8 = 2;
}

/// Xet protocol spec version openxet implements (design section 3).
const PROTOCOL_SPEC: &str = "1.1.0";

#[derive(Parser)]
#[command(
    name = "openxet",
    version,
    about = "Open-source server and client for the Xet protocol"
)]
struct Cli {
    /// Print machine-readable JSON instead of text
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print version and build information
    Version,
}

#[derive(Serialize)]
struct VersionInfo {
    schema: &'static str,
    version: &'static str,
    git_commit: &'static str,
    protocol_spec: &'static str,
}

impl HumanDisplay for VersionInfo {
    fn human(&self) -> String {
        format!(
            "openxet {} (commit {}, xet protocol {})",
            self.version, self.git_commit, self.protocol_spec
        )
    }
}

#[derive(Serialize)]
struct UsageError {
    schema: &'static str,
    code: &'static str,
    message: String,
    hint: &'static str,
}

/// Parses `args` and runs the command, returning the process exit code.
pub fn run(args: impl IntoIterator<Item = String>) -> u8 {
    let args: Vec<String> = args.into_iter().collect();
    let json_requested = args.iter().any(|arg| arg == "--json");

    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) if err.use_stderr() => return report_usage_error(&err, json_requested),
        Err(err) => {
            // --help and --version: clap prints to stdout and succeeds.
            let _ = err.print();
            return ExitCode::SUCCESS;
        }
    };

    let output = Output::from_json_flag(cli.json);
    let result = match cli.command {
        Command::Version => output.emit(&VersionInfo {
            schema: "openxet.version.v1",
            version: env!("CARGO_PKG_VERSION"),
            git_commit: env!("OPENXET_GIT_COMMIT"),
            protocol_spec: PROTOCOL_SPEC,
        }),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => report_io_error(&err),
    }
}

fn report_usage_error(err: &clap::Error, json: bool) -> u8 {
    if json {
        let message = err.to_string();
        let message = message.lines().next().unwrap_or_default();
        let error = UsageError {
            schema: "openxet.error.v1",
            code: "usage",
            message: message
                .strip_prefix("error: ")
                .unwrap_or(message)
                .to_owned(),
            hint: "Run `openxet --help` to list commands and options.",
        };
        if let Ok(line) = serde_json::to_string(&error) {
            eprintln!("{line}");
        }
    } else {
        let _ = err.print();
    }
    ExitCode::USAGE
}

fn report_io_error(err: &io::Error) -> u8 {
    eprintln!("error: could not write output: {err}");
    ExitCode::FAILURE
}
