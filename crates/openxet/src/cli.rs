use std::ffi::OsString;

use clap::{CommandFactory, Parser, Subcommand};
use openxet_protocol::PROTOCOL_SPEC;
use serde::Serialize;

use crate::config_cmd::{self, ConfigCommand};
use crate::error::{CliError, ExitCode};
use crate::label_cmd::{self, LabelCommand};
use crate::openapi_cmd;
use crate::output::{HumanDisplay, Output};
use crate::purge_cmd::{self, PurgeArgs};
use crate::serve::{self, ServeArgs};
use crate::token_cmd::{self, TokenCommand};
use crate::transfer_cmd::{self, DownloadArgs, UploadArgs};

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
    /// Serve the Xet CAS; without a config file, in dev mode with a generated token
    Serve(ServeArgs),
    /// Check and explain the server configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Create CAS-token signing keys, CAS tokens and static tokens
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Upload files to a Xet CAS and print their Xet hashes
    Upload(UploadArgs),
    /// Download a file from a Xet CAS by its Xet hash
    Download(DownloadArgs),
    /// Unregister a file from every scope, in the server's data directory
    Purge(PurgeArgs),
    /// Print the server's OpenAPI 3.1 document, as GET /openapi.json serves it
    Openapi,
    /// Add, remove and list a repository's labels on a running server
    Label {
        #[command(subcommand)]
        command: LabelCommand,
    },
    /// The proc-receive hook of the pushes openxet serves; git runs it
    #[command(hide = true)]
    ProcReceive,
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
                return error.report(true);
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
    let result = match &cli.command {
        Command::Version => output
            .emit(&VersionInfo {
                schema: "openxet.version.v1",
                version: env!("CARGO_PKG_VERSION"),
                git_commit: env!("OPENXET_GIT_COMMIT"),
                protocol_spec: PROTOCOL_SPEC,
            })
            .map_err(|err| CliError::output(&err)),
        Command::Serve(args) => serve::run(args, output),
        Command::Config { command } => config_cmd::run(command, output),
        Command::Token { command } => token_cmd::run(command, output),
        Command::Upload(args) => transfer_cmd::upload(args, output),
        Command::Download(args) => transfer_cmd::download(args, output),
        Command::Purge(args) => purge_cmd::run(args, output),
        Command::Label { command } => label_cmd::run(command, output),
        Command::Openapi => openapi_cmd::run(output),
        Command::ProcReceive => return crate::hook_cmd::run(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => error.report(json),
    }
}

/// Whether `--json` is among the options, so even a parse failure can be
/// reported as JSON. Matches clap 4's reading: arguments after `--` are
/// positional, and the argument after an option that takes a value is that
/// value, unless it starts with `-` and the option doesn't allow hyphen
/// values (clap then reports the value as missing).
fn json_requested(cmd: &clap::Command, args: &[OsString]) -> bool {
    // `Some(allows hyphen values)` if `arg` names an option that takes one.
    let value_option = |arg: &str| {
        let mut cmds = vec![cmd];
        while let Some(cmd) = cmds.pop() {
            cmds.extend(cmd.get_subcommands());
            let found = cmd.get_arguments().find(|a| {
                a.get_action().takes_values()
                    && (a
                        .get_long()
                        .is_some_and(|l| arg.strip_prefix("--") == Some(l))
                        || a.get_short().is_some_and(|s| arg == format!("-{s}")))
            });
            if let Some(a) = found {
                return Some(a.is_allow_hyphen_values_set());
            }
        }
        None
    };
    let mut args = args
        .iter()
        .skip(1)
        .map(|arg| arg.to_string_lossy())
        .peekable();
    while let Some(arg) = args.next() {
        match arg.as_ref() {
            "--" => return false,
            "--json" => return true,
            arg => {
                if let Some(hyphen_values) = value_option(arg)
                    && args
                        .peek()
                        .is_some_and(|next| hyphen_values || !next.starts_with('-'))
                {
                    args.next();
                }
            }
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
    fn json_after_a_value_option_is_the_flag_unless_hyphen_values_are_allowed() {
        let cmd = Cli::command()
            .subcommand(clap::Command::new("put").arg(clap::Arg::new("out").long("out").short('o')))
            .subcommand(
                clap::Command::new("run")
                    .arg(clap::Arg::new("arg").long("arg").allow_hyphen_values(true)),
            );
        // clap rejects `--out --json` as a missing value, and the caller
        // asked for JSON, so the error must be JSON.
        assert!(json_requested(
            &cmd,
            &args(&["openxet", "put", "--out", "--json"])
        ));
        assert!(json_requested(
            &cmd,
            &args(&["openxet", "put", "-o", "--json"])
        ));
        assert!(json_requested(
            &cmd,
            &args(&["openxet", "put", "--out", "x", "--json"])
        ));
        assert!(!json_requested(
            &cmd,
            &args(&["openxet", "put", "--out", "json", "x"])
        ));
        assert!(!json_requested(
            &cmd,
            &args(&["openxet", "run", "--arg", "--json"])
        ));
    }

    /// Every command's invocation as the agent leg lists it: `["name"` for a
    /// top-level command, `["parent", "child"` for a nested one. Hidden
    /// commands are skipped: `proc-receive` is git's hook, not a command
    /// people or agents run.
    fn invocations(cmd: &clap::Command, parents: &[String]) -> Vec<String> {
        let mut found = Vec::new();
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()) {
            let mut path = parents.to_vec();
            path.push(format!("\"{}\"", sub.get_name()));
            if sub.has_subcommands() {
                found.extend(invocations(sub, &path));
            } else {
                found.push(format!("[{}", path.join(", ")));
            }
        }
        found
    }

    const AGENT_LEG: &str = include_str!("../../../compat/tests/test_agent_cli.py");

    /// The agent leg (`compat/tests/test_agent_cli.py`) runs every command
    /// non-interactively with `--json`; a new subcommand, nested ones
    /// included, must be added there.
    #[test]
    fn every_subcommand_is_in_the_agent_leg() {
        let missing: Vec<String> = invocations(&Cli::command(), &[])
            .into_iter()
            .filter(|invocation| !AGENT_LEG.contains(invocation.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "add {missing:?} to JSON_COMMANDS in compat/tests/test_agent_cli.py"
        );
    }

    #[test]
    fn agent_leg_lists_new_commands() {
        let listed = invocations(&Cli::command(), &[]);
        for invocation in [
            r#"["serve""#,
            r#"["config", "check""#,
            r#"["token", "keygen""#,
            r#"["token", "mint""#,
            r#"["token", "generate""#,
            r#"["upload""#,
            r#"["download""#,
            r#"["purge""#,
            r#"["label", "add""#,
            r#"["label", "remove""#,
            r#"["label", "list""#,
        ] {
            assert!(
                listed.iter().any(|l| l == invocation),
                "{invocation} is not a command"
            );
            assert!(
                AGENT_LEG.contains(invocation),
                "{invocation} is not in the agent leg"
            );
        }
    }
}
