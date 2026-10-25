//! The `proc-receive` hook the tests install in place of `openxet`, which
//! the `openxet-git` tests can't build. Without a mode file it does exactly
//! what `openxet proc-receive` does. A test forges what it relays by
//! writing `{git.cache_dir}/test-hook-mode` (the hook runs as
//! `{git.cache_dir}/hooks/proc-receive`), one directive per line:
//!
//! - `extra <line>`: add a request line, such as one naming a repository
//! - `pack <path>`: report this pack path instead of the real ones; a path
//!   starting with `./` is joined to the hook's directory unresolved
//! - `no-pack`: report no pack path
//! - `symlink <name> <target>`: first create `<name>` in the hook's
//!   directory (the request's snapshot) as a symlink to `<target>`
//! - `old <oid>`: relay every command with this old value
//! - `token <value>`: present this token; `no-token`: present none
//! - `replay <file>`: send the request a second time after the answer and
//!   write what the second attempt got to `<file>`
//!
//! `openxet-git-test-hook list-fds` prints the descriptors it holds, one
//! `<fd> <kind>` line each, for the channel-privacy test, and
//! `openxet-git-test-hook exchange` sends one request with the token from
//! its environment and prints the reply lines, or `error …`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{self, Write as _};
use std::os::unix::fs::FileTypeExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use openxet_git::hook::{HookChannel, negotiate, new_packs, report, request_lines};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("list-fds") => {
            list_fds();
            return ExitCode::SUCCESS;
        }
        Some("exchange") => {
            exchange();
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
    let mode = Path::new(&args[0])
        .parent()
        .and_then(Path::parent)
        .map(|cache| cache.join("test-hook-mode"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    match run(&mode) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(io::stderr(), "openxet proc-receive: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(mode: &str) -> io::Result<()> {
    let mut channel = HookChannel::from_env()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut commands = negotiate(&mut input, &mut output)?;
    let mut packs = new_packs(Path::new("."));
    let mut token = Some(channel.token().to_owned());
    let mut extra = Vec::new();
    let mut replay_to = None;
    for line in mode.lines() {
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "extra" => extra.push(value.as_bytes().to_vec()),
            "pack" => {
                let path = match value.strip_prefix("./") {
                    Some(rel) => std::env::current_dir()?.join(rel),
                    None => PathBuf::from(value),
                };
                packs = vec![path];
            }
            "no-pack" => packs.clear(),
            "symlink" => {
                let (name, target) = value.split_once(' ').unwrap_or((value, ""));
                std::os::unix::fs::symlink(target, name)?;
            }
            "old" => {
                for c in &mut commands {
                    let rest = c[41..].to_vec();
                    *c = [value.as_bytes(), b" ", &rest].concat();
                }
            }
            "token" => token = Some(value.to_owned()),
            "no-token" => token = None,
            "replay" => replay_to = Some(PathBuf::from(value)),
            _ => {}
        }
    }
    let mut request = request_lines(token.as_deref().unwrap_or(""), &commands, &packs);
    if token.is_none() {
        request.remove(0);
    }
    request.extend(extra);
    let reply = channel.exchange(&request)?;
    if let Some(file) = replay_to {
        let again = match channel.exchange(&request) {
            Ok(lines) => lines
                .iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => format!("error {e}"),
        };
        std::fs::write(file, again)?;
    }
    report(&reply, &mut output, &mut io::stderr().lock())
}

fn exchange() {
    let zero = "0".repeat(40);
    let result = HookChannel::from_env().and_then(|mut channel| {
        let command = format!("{zero} {} refs/heads/probe", "a".repeat(40)).into_bytes();
        let request = request_lines(channel.token(), &[command], &[]);
        channel.exchange(&request)
    });
    let mut out = io::stdout().lock();
    match result {
        Ok(lines) => {
            for line in lines {
                let _ = writeln!(out, "{}", String::from_utf8_lossy(&line));
            }
        }
        Err(e) => {
            let _ = writeln!(out, "error {e}");
        }
    }
}

fn list_fds() {
    let mut out = io::stdout().lock();
    for fd in 0..1024 {
        let Ok(meta) = std::fs::metadata(format!("/dev/fd/{fd}")) else {
            continue;
        };
        let t = meta.file_type();
        let kind = if t.is_fifo() {
            "fifo"
        } else if t.is_socket() {
            "socket"
        } else if t.is_dir() {
            "dir"
        } else if t.is_char_device() {
            "char"
        } else {
            "file"
        };
        let _ = writeln!(out, "{fd} {kind}");
    }
}
