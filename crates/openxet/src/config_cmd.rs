//! `openxet config check`: validates a configuration and explains it.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{self, Overrides, Problem, Setting};
use crate::error::CliError;
use crate::output::{HumanDisplay, Output};

#[derive(Debug, clap::Subcommand)]
pub enum ConfigCommand {
    /// Check a configuration, with the environment's overrides, and list
    /// every problem or every effective setting
    Check {
        /// TOML configuration file; without one, the defaults and the
        /// environment are checked
        #[arg(long, value_name = "FILE")]
        config: Option<PathBuf>,
    },
}

/// The `openxet.config-check.v1` report.
#[derive(Serialize)]
struct Report {
    schema: &'static str,
    valid: bool,
    file: Option<String>,
    /// Whether the server would run in dev mode: no `[auth]`.
    dev_mode: Option<bool>,
    problems: Vec<Problem>,
    settings: Vec<Setting>,
}

impl HumanDisplay for Report {
    fn human(&self) -> String {
        let file = self
            .file
            .as_deref()
            .unwrap_or("no file; defaults and environment");
        let mut out = String::new();
        if self.valid {
            let mode = if self.dev_mode == Some(true) {
                "dev mode"
            } else {
                "configured credentials"
            };
            let _ = writeln!(out, "The configuration is valid ({file}; {mode}).\n");
            for setting in &self.settings {
                let _ = writeln!(
                    out,
                    "  {} = {}  ({})",
                    setting.key, setting.value, setting.source
                );
            }
        } else {
            let _ = writeln!(
                out,
                "The configuration has {} problem(s) ({file}):",
                self.problems.len()
            );
            for problem in &self.problems {
                let value = problem
                    .value
                    .as_deref()
                    .map(|v| format!(" = {v:?}"))
                    .unwrap_or_default();
                let _ = write!(
                    out,
                    "\n  {}{value}  ({})\n    rule: {}\n    hint: {}\n",
                    problem.key, problem.source, problem.rule, problem.hint
                );
            }
        }
        out.trim_end().to_owned()
    }
}

pub fn run(command: &ConfigCommand, output: Output) -> Result<(), CliError> {
    let ConfigCommand::Check { config: file } = command;
    let loaded = config::load(file.as_deref(), std::env::vars_os(), &Overrides::default());
    let report = Report {
        schema: "openxet.config-check.v1",
        valid: loaded.problems.is_empty(),
        file: loaded.file.map(|f| f.display().to_string()),
        dev_mode: loaded.config.as_ref().map(|c| c.auth.is_none()),
        problems: loaded.problems,
        settings: loaded.settings,
    };
    output.emit(&report).map_err(|err| CliError::output(&err))?;
    if report.valid {
        Ok(())
    } else {
        Err(CliError::new(
            "config",
            format!("the configuration has {} problem(s)", report.problems.len()),
            "Fix each problem listed on stdout, then run `openxet config check` again.",
        ))
    }
}
