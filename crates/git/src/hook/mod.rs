//! The `proc-receive` hook and its channel to the openxet process that runs
//! the push (ADR 0004 items 3 and 7).
//!
//! receive-pack hands every ref command to the `proc-receive` hook
//! (`receive.procReceiveRefs=refs/`), which is openxet itself
//! (`openxet proc-receive`, [`proc_receive_hook`]). The hook speaks git's
//! proc-receive protocol on stdin and stdout (githooks(5), "proc-receive")
//! and relays the commands and the new pack's path to the parent over a
//! private channel the git child inherits, then relays the parent's `ok` or
//! `ng` back to git.
//!
//! The channel is a pair of pipes, created per request with `FD_CLOEXEC`
//! set and cleared only in the git child (and so in its hook), found
//! through `OPENXET_HOOK_FD` (`{read},{write}`). It has no filesystem path
//! and no listener. A pipe rather than a socketpair: the hook adopts the
//! inherited descriptors by opening `/dev/fd/N`, which works for pipes on
//! Linux and macOS but not for sockets on Linux, and safe Rust has no other
//! way to take ownership of an inherited descriptor number.
//!
//! The parent trusts nothing the hook supplies but the commands and the
//! pack path: the request must carry the request's single-use token
//! (`OPENXET_HOOK_TOKEN`), and the repository and principal come from the
//! token's [`HookContext`] only. Unknown lines are ignored.

mod client;
mod server;

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

use openxet_index::{Oid, RepoId};

pub use client::{HookChannel, negotiate, new_packs, proc_receive_hook, report, request_lines};
pub(crate) use server::confine;
pub use server::{HookServer, Served};

/// The variable naming the hook channel's descriptors, `{read},{write}` as
/// the hook sees them.
pub const HOOK_FD_ENV: &str = "OPENXET_HOOK_FD";
/// The variable carrying the request's single-use token.
pub const HOOK_TOKEN_ENV: &str = "OPENXET_HOOK_TOKEN";

/// The largest pkt-line, length prefix included (gitprotocol-common(5)).
pub(crate) const MAX_PKT_LEN: usize = 65520;
/// The most lines one hook request may hold: a push of this many refs is
/// far past any client's.
pub(crate) const MAX_REQUEST_LINES: usize = 100_000;

/// Who a request's hook acts for: fixed by the parent when the request
/// starts, never taken from the hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookContext {
    /// The repository the request pushes to.
    pub repo: RepoId,
    /// The pusher's subject, for the ref log.
    pub actor: String,
    /// The request id, for the ref log.
    pub request: String,
}

/// One ref command as the hook relayed it, parsed but not yet checked
/// against policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCommand {
    /// The ref's target the client saw; `None` to create it.
    pub old: Option<Oid>,
    /// The new target; `None` to delete it.
    pub new: Option<Oid>,
    /// The ref.
    pub name: String,
}

/// What the hook relayed: the commands and the pack paths it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRequest {
    /// The ref commands, in git's order.
    pub commands: Vec<HookCommand>,
    /// Paths of the packs the hook found receive-pack writing, unchecked.
    pub packs: Vec<PathBuf>,
}

/// What the parent answers the hook: lines for the pusher's terminal and
/// one result per command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HookReply {
    /// Shown to the pusher as `remote: …` lines.
    pub messages: Vec<String>,
    /// Per ref: `Ok` for `ok`, `Err(reason)` for `ng`.
    pub results: Vec<(String, Result<(), String>)>,
}

/// Parses the lines after the token: `command <old> <new> <ref>` and
/// `pack <path>`. Every other line is ignored, so a hook can't name a
/// repository or principal.
pub(crate) fn parse_request(lines: &[Vec<u8>]) -> Result<HookRequest, String> {
    let mut request = HookRequest {
        commands: Vec::new(),
        packs: Vec::new(),
    };
    for line in lines {
        if let Some(path) = line.strip_prefix(b"pack ") {
            request.packs.push(PathBuf::from(OsStr::from_bytes(path)));
        } else if let Some(command) = line.strip_prefix(b"command ") {
            request.commands.push(parse_command(command).ok_or_else(|| {
                format!(
                    "malformed command from the hook: {:?}",
                    String::from_utf8_lossy(command)
                )
            })?);
        }
    }
    if request.commands.is_empty() {
        return Err("the hook relayed no command".to_owned());
    }
    Ok(request)
}

/// `<old> <new> <ref>`, the zero id meaning none.
fn parse_command(line: &[u8]) -> Option<HookCommand> {
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.splitn(3, ' ');
    let oid = |s: &str| -> Option<Option<Oid>> {
        let oid: Oid = s.parse().ok()?;
        Some((oid.0 != [0; 20]).then_some(oid))
    };
    let old = oid(parts.next()?)?;
    let new = oid(parts.next()?)?;
    let name = parts.next()?.to_owned();
    Some(HookCommand { old, new, name })
}

pub(crate) fn pkt_len(len: [u8; 4]) -> io::Result<Option<usize>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid pkt-line length");
    let text = std::str::from_utf8(&len).map_err(|_| invalid())?;
    let n = usize::from_str_radix(text, 16).map_err(|_| invalid())?;
    match n {
        0 => Ok(None),
        5..=MAX_PKT_LEN => Ok(Some(n - 4)),
        _ => Err(invalid()),
    }
}

pub(crate) fn encode_pkt(out: &mut Vec<u8>, payload: &[u8]) {
    out.extend_from_slice(format!("{:04x}", payload.len() + 4).as_bytes());
    out.extend_from_slice(payload);
}

/// Reads one pkt-line; `None` for a flush.
fn read_pkt(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let Some(n) = pkt_len(len)? else {
        return Ok(None);
    };
    let mut payload = vec![0u8; n];
    r.read_exact(&mut payload)?;
    Ok(Some(payload))
}

/// Reads pkt-lines up to a flush.
pub(crate) fn read_section(r: &mut impl Read) -> io::Result<Vec<Vec<u8>>> {
    let mut lines = Vec::new();
    while let Some(line) = read_pkt(r)? {
        if lines.len() == MAX_REQUEST_LINES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many pkt-lines",
            ));
        }
        lines.push(line);
    }
    Ok(lines)
}

pub(crate) fn write_section(w: &mut impl Write, lines: &[Vec<u8>]) -> io::Result<()> {
    let mut out = Vec::new();
    for line in lines {
        encode_pkt(&mut out, line);
    }
    out.extend_from_slice(b"0000");
    w.write_all(&out)?;
    w.flush()
}

pub(crate) fn trim_newline(mut line: Vec<u8>) -> Vec<u8> {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    line
}

/// Only with the `fuzzing` feature, for `fuzz/`: the parent's reading of
/// one hook request as it arrives on the channel, pkt-lines up to a flush,
/// with `token` as the request's token.
///
/// # Errors
///
/// The reason the parent would refuse the request.
#[cfg(feature = "fuzzing")]
pub fn parse_hook_request(mut bytes: &[u8], token: &str) -> Result<HookRequest, String> {
    let lines = read_section(&mut bytes).map_err(|e| format!("unreadable hook request: {e}"))?;
    server::check_request(token, &lines)
}

#[cfg(test)]
mod tests {
    use super::{parse_command, parse_request, pkt_len};

    #[test]
    fn commands_parse() {
        let zero = "0".repeat(40);
        let a = "a".repeat(40);
        let c = parse_command(format!("{zero} {a} refs/heads/main").as_bytes())
            .unwrap_or_else(|| panic!("parses"));
        assert_eq!(c.old, None);
        assert_eq!(c.new.map(|o| o.to_string()), Some(a.clone()));
        assert_eq!(c.name, "refs/heads/main");
        assert!(parse_command(b"nonsense").is_none());
        assert!(parse_command(format!("{a} {a}").as_bytes()).is_none());
    }

    #[test]
    fn unknown_lines_are_ignored() {
        let a = "a".repeat(40);
        let lines = vec![
            b"repo 0192f3a4b5c67d8e9fa0b1c2d3e4f5a6".to_vec(),
            format!("command {a} {a} refs/heads/x").into_bytes(),
            b"principal root".to_vec(),
        ];
        let request = parse_request(&lines).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(request.commands.len(), 1);
        assert!(parse_request(&[]).is_err(), "no command");
    }

    #[test]
    fn pkt_lengths() {
        assert_eq!(pkt_len(*b"0000").ok(), Some(None));
        assert_eq!(pkt_len(*b"0005").ok(), Some(Some(1)));
        assert!(pkt_len(*b"0004").is_err());
        assert!(pkt_len(*b"fff1").is_err());
        assert!(pkt_len(*b"zzzz").is_err());
    }
}
