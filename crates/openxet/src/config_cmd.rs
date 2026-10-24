//! `openxet config check`: validates a configuration and explains it.

use std::fmt::Write as _;
use std::path::PathBuf;

use serde::Serialize;

use crate::config::{self, Overrides, Problem, Setting};
use crate::error::CliError;
use crate::output::{HumanDisplay, Output};
use crate::preflight;

#[derive(Debug, clap::Subcommand)]
pub enum ConfigCommand {
    /// Check a configuration, with the environment's overrides, and the
    /// data directory and key files it names; list every problem or every
    /// effective setting
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
    /// What `serve` accepts but warns about, such as a data directory other
    /// users may read.
    warnings: Vec<Problem>,
    settings: Vec<Setting>,
}

fn write_problem(out: &mut String, problem: &Problem) {
    let value = match &problem.value {
        Some(v) if problem.quoted => format!(" = {v:?}"),
        Some(v) => format!(" = {v}"),
        None => String::new(),
    };
    let _ = write!(
        out,
        "\n  {}{value}  ({})\n    rule: {}\n    hint: {}\n",
        problem.key, problem.source, problem.rule, problem.hint
    );
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
                write_problem(&mut out, problem);
            }
        }
        if !self.warnings.is_empty() {
            let _ = writeln!(out, "\n{} warning(s):", self.warnings.len());
            for warning in &self.warnings {
                write_problem(&mut out, warning);
            }
        }
        out.trim_end().to_owned()
    }
}

pub fn run(command: &ConfigCommand, output: Output) -> Result<(), CliError> {
    let ConfigCommand::Check { config: file } = command;
    let loaded = config::load(file.as_deref(), std::env::vars_os(), &Overrides::default());
    let mut problems = loaded.problems;
    let mut warnings = Vec::new();
    if let Some(config) = &loaded.config {
        let findings = preflight::check(config);
        problems.extend(findings.problems);
        warnings = findings.warnings;
    }
    let report = Report {
        schema: "openxet.config-check.v1",
        valid: problems.is_empty(),
        file: loaded.file.map(|f| f.display().to_string()),
        dev_mode: loaded
            .config
            .as_ref()
            .filter(|_| problems.is_empty())
            .map(|c| c.auth.is_none()),
        problems,
        warnings,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_values_are_shown_as_written_and_text_quoted() {
        let problem = |value: &str, quoted| Problem {
            key: "k".to_owned(),
            value: Some(value.to_owned()),
            source: "default".to_owned(),
            rule: "r".to_owned(),
            hint: "h".to_owned(),
            quoted,
        };
        let mut out = String::new();
        write_problem(&mut out, &problem("0", false));
        write_problem(&mut out, &problem("5m", true));
        assert!(out.contains("k = 0  ("), "{out}");
        assert!(out.contains("k = \"5m\"  ("), "{out}");
    }
}
