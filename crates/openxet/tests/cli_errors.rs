//! The CLI error contract (`docs/reference/exit-codes.md`): stable exit
//! codes, and with `--json` one `openxet.error.v1` object on stderr.

// The helpers are test code too; a panic is the intended failure.
#![allow(clippy::unwrap_used)]

use std::io;
use std::process::{self, Output, Stdio};

use assert_cmd::{Command, cargo::cargo_bin_cmd};

fn openxet() -> Command {
    cargo_bin_cmd!("openxet")
}

/// Runs `openxet args` with stdout on a pipe nobody reads, so every write
/// fails with a broken pipe.
fn with_unwritable_stdout(args: &[&str]) -> Output {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    process::Command::new(env!("CARGO_BIN_EXE_openxet"))
        .args(args)
        .stdout(Stdio::from(writer))
        .output()
        .unwrap()
}

fn json_error(out: &Output) -> serde_json::Value {
    let error: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap_or_else(|err| {
        panic!(
            "stderr is not one JSON object ({err}): {}",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(error["schema"], "openxet.error.v1");
    assert!(error["message"].is_string());
    assert!(error["hint"].is_string());
    error
}

#[cfg(unix)]
#[test]
fn non_utf8_argument_is_a_usage_error_not_a_panic() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let arg = OsStr::from_bytes(b"\xff");
    let out = openxet().arg(arg).arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_error(&out)["code"], "usage");

    let out = openxet().arg(arg).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("panicked"));
}

#[test]
fn output_failure_with_json_is_a_structured_error() {
    let out = with_unwritable_stdout(&["version", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let error = json_error(&out);
    assert_eq!(error["code"], "io");
}

#[test]
fn output_failure_without_json_is_a_line_of_text() {
    let out = with_unwritable_stdout(&["version"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: "));
}

#[test]
fn json_after_double_dash_is_an_argument_not_the_flag() {
    let out = openxet()
        .args(["version", "--", "--json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(serde_json::from_slice::<serde_json::Value>(&out.stderr).is_err());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--json"));
}

#[test]
fn help_exits_zero_on_stdout() {
    let out = openxet().arg("--help").output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage: openxet"));
}

#[test]
fn bare_openxet_prints_help_and_exits_with_usage_code() {
    let out = openxet().output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("Usage: openxet"));

    let out = openxet().arg("--json").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json_error(&out)["code"], "usage");
}
