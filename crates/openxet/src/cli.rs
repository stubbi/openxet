use std::ffi::OsString;
use std::io;

use clap::{CommandFactory, Parser, Subcommand};
use openxet_protocol::PROTOCOL_SPEC;
use serde::Serialize;

use crate::output::{HumanDisplay, Output};

/// Process exit codes, documented in `docs/reference/exit-codes.md`.
pub struct ExitCode;

impl ExitCode {
    pub const SUCCESS: u8 = 0;
    pub const FAILURE: u8 = 1;
    pub const USAGE: u8 = 2;
}

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

/// A failure reported to the user: one line of text, or with `--json` one
/// `openxet.error.v1` object, on stderr.
#[derive(Serialize)]
struct CliError {
    schema: &'static str,
    /// Stable machine-readable kind, e.g. `usage` or `io`.
    code: &'static str,
    message: String,
    /// The next action to take.
    hint: &'static str,
    #[serde(skip)]
    exit: u8,
}

impl CliError {
    fn usage(err: &clap::Error) -> Self {
        let rendered = err.to_string();
        let message = rendered.lines().next().unwrap_or_default();
        Self {
            schema: "openxet.error.v1",
            code: "usage",
            message: message
                .strip_prefix("error: ")
                .unwrap_or(message)
                .to_owned(),
            hint: "Run `openxet --help` to list commands and options.",
            exit: ExitCode::USAGE,
        }
    }

    fn output(err: &io::Error) -> Self {
        Self {
            schema: "openxet.error.v1",
            code: "io",
            message: format!("could not write output: {err}"),
            hint: "Check that stdout is writable, for example that the reading end of a pipe \
                   stays open.",
            exit: ExitCode::FAILURE,
        }
    }

    /// Prints the error with `--json`; the caller prints the text form.
    fn report_json(&self) -> u8 {
        if let Ok(line) = serde_json::to_string(self) {
            eprintln!("{line}");
        }
        self.exit
    }
}

/// Parses `args` and runs the command, returning the process exit code.
/// Arguments need not be UTF-8: clap reports one it can't use as a usage
/// error.
pub fn run(args: impl IntoIterator<Item = OsString>) -> u8 {
    let args: Vec<OsString> = args.into_iter().collect();
    let json = json_requested(&Cli::command(), &args);

    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) if err.use_stderr() => {
            let error = CliError::usage(&err);
            if json {
                return error.report_json();
            }
            let _ = err.print();
            return error.exit;
        }
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
        Err(err) => {
            let error = CliError::output(&err);
            if json {
                return error.report_json();
            }
            eprintln!("error: {}", error.message);
            error.exit
        }
    }
}

/// Whether `--json` is among the options, so even a parse failure can be
/// reported as JSON. Matches clap's reading: arguments after `--` are
/// positional, and the argument after an option that takes a value is that
/// value.
fn json_requested(cmd: &clap::Command, args: &[OsString]) -> bool {
    let takes_value = |arg: &str| {
        let mut cmds = vec![cmd];
        while let Some(cmd) = cmds.pop() {
            cmds.extend(cmd.get_subcommands());
            let found = cmd.get_arguments().any(|a| {
                a.get_action().takes_values()
                    && (a
                        .get_long()
                        .is_some_and(|l| arg.strip_prefix("--") == Some(l))
                        || a.get_short().is_some_and(|s| arg == format!("-{s}")))
            });
            if found {
                return true;
            }
        }
        false
    };
    let mut args = args.iter().skip(1).map(|arg| arg.to_string_lossy());
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--" => return false,
            "--json" => return true,
            arg if takes_value(arg) => {
                args.next();
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn json_flag_is_found_before_double_dash_only() {
        let cmd = Cli::command();
        assert!(json_requested(
            &cmd,
            &args(&["openxet", "version", "--json"])
        ));
        assert!(json_requested(&cmd, &args(&["openxet", "--json", "nope"])));
        assert!(!json_requested(
            &cmd,
            &args(&["openxet", "version", "--", "--json"])
        ));
        assert!(!json_requested(&cmd, &args(&["openxet", "version"])));
    }

    #[test]
    fn json_as_an_option_value_is_not_the_flag() {
        let cmd = Cli::command().subcommand(
            clap::Command::new("put").arg(clap::Arg::new("out").long("out").short('o')),
        );
        assert!(!json_requested(
            &cmd,
            &args(&["openxet", "put", "--out", "--json"])
        ));
        assert!(!json_requested(
            &cmd,
            &args(&["openxet", "put", "-o", "--json"])
        ));
        assert!(json_requested(
            &cmd,
            &args(&["openxet", "put", "--out", "x", "--json"])
        ));
    }

    /// The agent leg (`compat/tests/test_agent_cli.py`) runs every command
    /// non-interactively with `--json`; a new subcommand must be added there.
    #[test]
    fn every_subcommand_is_in_the_agent_leg() {
        let agent_leg = include_str!("../../../compat/tests/test_agent_cli.py");
        let cli = Cli::command();
        let missing: Vec<&str> = cli
            .get_subcommands()
            .map(clap::Command::get_name)
            .filter(|name| !agent_leg.contains(&format!("[\"{name}\"")))
            .collect();
        assert!(
            missing.is_empty(),
            "add {missing:?} to JSON_COMMANDS in compat/tests/test_agent_cli.py"
        );
    }
}
