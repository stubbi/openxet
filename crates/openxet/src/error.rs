use std::borrow::Cow;
use std::io::{self, Write};

use serde::Serialize;

/// Process exit codes, documented in `docs/reference/exit-codes.md`.
pub struct ExitCode;

impl ExitCode {
    pub const SUCCESS: u8 = 0;
    pub const FAILURE: u8 = 1;
    pub const USAGE: u8 = 2;
    /// Stopped by `SIGINT` or `SIGTERM`, as shells report 128 + `SIGINT`.
    pub const INTERRUPTED: u8 = 130;
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
    /// Per-file problems, for a command given several paths; absent when
    /// empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileError>,
    #[serde(skip)]
    pub exit: u8,
}

/// One path's problem in [`CliError::files`].
#[derive(Debug, Serialize)]
pub struct FileError {
    pub path: String,
    /// Stable machine-readable kind, e.g. `not_found`.
    pub problem: &'static str,
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
            files: Vec::new(),
            exit: ExitCode::FAILURE,
        }
    }

    /// The same error with exit code `exit`.
    #[must_use]
    pub fn with_exit(self, exit: u8) -> Self {
        Self { exit, ..self }
    }

    /// The same error with per-file problems.
    #[must_use]
    pub fn with_files(self, files: Vec<FileError>) -> Self {
        Self { files, ..self }
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
            let _ = writeln!(stderr, "error: {}", self.message);
            for file in &self.files {
                let _ = writeln!(stderr, "  {}: {}", file.path, file.problem);
            }
            let _ = writeln!(stderr, "hint: {}", self.hint);
        }
        self.exit
    }
}
