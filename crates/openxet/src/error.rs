use std::borrow::Cow;
use std::io::{self, Write};

use serde::Serialize;

/// Process exit codes, documented in `docs/reference/exit-codes.md`.
pub struct ExitCode;

impl ExitCode {
    pub const SUCCESS: u8 = 0;
    pub const FAILURE: u8 = 1;
    pub const USAGE: u8 = 2;
}

/// A failure reported to the user: one line of text, or with `--json` one
/// `openxet.error.v1` object, on stderr. Codes are listed in
/// `docs/reference/exit-codes.md`.
///
/// Invariant: no message or hint ever holds a credential.
#[derive(Debug, Serialize)]
pub struct CliError {
    schema: &'static str,
    /// Stable machine-readable kind, e.g. `usage` or `io`.
    pub code: &'static str,
    pub message: String,
    /// The next action to take.
    pub hint: Cow<'static, str>,
    #[serde(skip)]
    pub exit: u8,
}

impl CliError {
    /// A failure of a valid command (exit 1).
    pub fn new(
        code: &'static str,
        message: impl Into<String>,
        hint: impl Into<Cow<'static, str>>,
    ) -> Self {
        Self {
            schema: "openxet.error.v1",
            code,
            message: message.into(),
            hint: hint.into(),
            exit: ExitCode::FAILURE,
        }
    }

    /// clap's message, on one line: its first paragraph, which names
    /// what is wrong (a missing argument is listed on the lines after the
    /// first), without the usage line and the `--help` tip that follow.
    pub fn usage(err: &clap::Error) -> Self {
        let rendered = err.to_string();
        let message = rendered
            .lines()
            .take_while(|line| !line.trim().is_empty())
            .map(str::trim)
            .collect::<Vec<_>>()
            .join(" ");
        Self {
            exit: ExitCode::USAGE,
            ..Self::new(
                "usage",
                message.strip_prefix("error: ").unwrap_or(&message),
                "Run `openxet --help` to list commands and options.",
            )
        }
    }

    pub fn output(err: &io::Error) -> Self {
        Self::new(
            "io",
            format!("could not write output: {err}"),
            "Check that stdout is writable, for example that the reading end of a pipe \
             stays open.",
        )
    }

    /// Prints the error on stderr, as JSON with `--json` and as
    /// `error: <message>` and `hint: <hint>` lines otherwise, and returns
    /// its exit code.
    pub fn report(&self, json: bool) -> u8 {
        let mut stderr = io::stderr().lock();
        // A closed stderr leaves nothing to report to; keep the exit code.
        if json {
            if let Ok(line) = serde_json::to_string(self) {
                let _ = writeln!(stderr, "{line}");
            }
        } else {
            let _ = writeln!(stderr, "error: {}\nhint: {}", self.message, self.hint);
        }
        self.exit
    }
}
