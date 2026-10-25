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

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use command_fds::CommandFdExt as _;
use openxet_index::{Oid, RepoId};
use subtle::ConstantTimeEq as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::unix::pipe;

use crate::GitError;

/// The variable naming the hook channel's descriptors, `{read},{write}` as
/// the hook sees them.
pub const HOOK_FD_ENV: &str = "OPENXET_HOOK_FD";
/// The variable carrying the request's single-use token.
pub const HOOK_TOKEN_ENV: &str = "OPENXET_HOOK_TOKEN";

/// The largest pkt-line, length prefix included (gitprotocol-common(5)).
const MAX_PKT_LEN: usize = 65520;
/// The most lines one hook request may hold: a push of this many refs is
/// far past any client's.
const MAX_REQUEST_LINES: usize = 100_000;

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

/// How a [`HookServer`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Served {
    /// The hook's request was handled and answered.
    Answered,
    /// The handler gave no answer (a fault-injected crash).
    Abandoned,
    /// The request was refused before the handler ran; the reason was sent
    /// to the hook.
    Rejected(String),
    /// The channel closed without a request: receive-pack never ran the
    /// hook, as when unpacking failed.
    NoRequest,
}

/// The parent's end of one request's hook channel, with the request's
/// single-use token. Dropping it closes the channel, which expires the
/// token.
#[derive(Debug)]
pub struct HookServer {
    context: HookContext,
    token: String,
    from_hook: pipe::Receiver,
    to_hook: pipe::Sender,
    /// The hook's ends: what it reads, what it writes. Moved into the git
    /// child by [`HookServer::attach`].
    child: Option<(OwnedFd, OwnedFd)>,
}

impl HookServer {
    /// Creates the channel and a fresh token for `context`.
    ///
    /// # Errors
    ///
    /// [`GitError::Io`] if the pipes can't be made.
    pub fn new(context: HookContext) -> Result<Self, GitError> {
        let pipe_err = |e| GitError::Io {
            op: "create the hook channel",
            path: PathBuf::new(),
            source: e,
        };
        let (hook_reads, to_hook) = io::pipe().map_err(pipe_err)?;
        let (from_hook, hook_writes) = io::pipe().map_err(pipe_err)?;
        let from_hook =
            pipe::Receiver::from_owned_fd(OwnedFd::from(from_hook)).map_err(pipe_err)?;
        let to_hook = pipe::Sender::from_owned_fd(OwnedFd::from(to_hook)).map_err(pipe_err)?;
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|e| pipe_err(io::Error::other(e.to_string())))?;
        Ok(Self {
            context,
            token: hex(&secret),
            from_hook,
            to_hook,
            child: Some((OwnedFd::from(hook_reads), OwnedFd::from(hook_writes))),
        })
    }

    /// The context the token is bound to.
    #[must_use]
    pub fn context(&self) -> &HookContext {
        &self.context
    }

    /// Hands the hook's ends of the channel to `cmd`'s child and sets
    /// `OPENXET_HOOK_FD` and `OPENXET_HOOK_TOKEN`. The descriptors keep
    /// `FD_CLOEXEC` in this process, so no other child inherits them; only
    /// `cmd`'s child gets them without it. Call it once, on the receive-pack
    /// command; the parent's copies close when `cmd` is dropped.
    pub fn attach(&mut self, cmd: &mut tokio::process::Command) {
        if let Some((reads, writes)) = self.child.take() {
            cmd.env(
                HOOK_FD_ENV,
                format!("{},{}", reads.as_raw_fd(), writes.as_raw_fd()),
            )
            .env(HOOK_TOKEN_ENV, &self.token)
            .preserved_fds(vec![reads, writes]);
        }
    }

    /// Serves the channel until it closes: the first request, if its token
    /// matches, goes to `handle`, whose reply is sent back; a request with
    /// a missing or wrong token, and every request after the first, is
    /// answered with an error. `handle` returning `None` sends nothing, as
    /// a crashed node would.
    pub async fn serve<F, Fut>(mut self, handle: F) -> Served
    where
        F: FnOnce(HookContext, HookRequest) -> Fut,
        Fut: Future<Output = Option<HookReply>>,
    {
        let lines = match self.read_request().await {
            Ok(Some(lines)) => lines,
            Ok(None) => return Served::NoRequest,
            Err(e) => return Served::Rejected(format!("unreadable hook request: {e}")),
        };
        let outcome = match self.check(&lines) {
            Err(reason) => {
                let _ = self.send(&[error_line(&reason)]).await;
                Served::Rejected(reason)
            }
            Ok(request) => match handle(self.context.clone(), request).await {
                Some(reply) => {
                    let _ = self.send(&reply_lines(&reply)).await;
                    Served::Answered
                }
                None => return Served::Abandoned,
            },
        };
        // The token is spent: anything else on the channel is a replay.
        while let Ok(Some(_)) = self.read_request().await {
            let _ = self
                .send(&[error_line("the hook token was already used")])
                .await;
        }
        outcome
    }

    fn check(&self, lines: &[Vec<u8>]) -> Result<HookRequest, String> {
        let presented = lines.first().and_then(|l| l.strip_prefix(b"token "));
        let valid = presented.is_some_and(|t| bool::from(t.ct_eq(self.token.as_bytes())));
        if !valid {
            return Err("the hook token is missing or wrong".to_owned());
        }
        parse_request(&lines[1..])
    }

    /// The next request's lines, up to its flush; `None` at end of channel.
    async fn read_request(&mut self) -> io::Result<Option<Vec<Vec<u8>>>> {
        let mut lines = Vec::new();
        loop {
            let mut len = [0u8; 4];
            match self.from_hook.read_exact(&mut len).await {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof && lines.is_empty() => {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
            let Some(n) = pkt_len(len)? else {
                return Ok(Some(lines));
            };
            let mut payload = vec![0u8; n];
            self.from_hook.read_exact(&mut payload).await?;
            if lines.len() == MAX_REQUEST_LINES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "too many lines in one hook request",
                ));
            }
            lines.push(payload);
        }
    }

    async fn send(&mut self, lines: &[Vec<u8>]) -> io::Result<()> {
        let mut out = Vec::new();
        for line in lines {
            encode_pkt(&mut out, line);
        }
        out.extend_from_slice(b"0000");
        self.to_hook.write_all(&out).await?;
        self.to_hook.flush().await
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn error_line(reason: &str) -> Vec<u8> {
    format!("error {reason}").into_bytes()
}

fn reply_lines(reply: &HookReply) -> Vec<Vec<u8>> {
    let mut lines: Vec<Vec<u8>> = reply
        .messages
        .iter()
        .map(|m| format!("message {}", one_line(m, MAX_MESSAGE)).into_bytes())
        .collect();
    for (name, result) in &reply.results {
        lines.push(match result {
            Ok(()) => format!("ok {name}").into_bytes(),
            Err(reason) => {
                // The hook relays `ng <ref> <reason>` to git in one pkt-line.
                let room = (MAX_PKT_LEN - 4).saturating_sub(name.len() + 4);
                format!("ng {name} {}", one_line(reason, room.min(MAX_MESSAGE))).into_bytes()
            }
        });
    }
    lines
}

/// The longest message or reason sent to the pusher, in bytes.
const MAX_MESSAGE: usize = 4096;

/// Keeps a message on one pkt-line of at most `max` bytes: control
/// characters would end it or garble the pusher's terminal.
fn one_line(text: &str, max: usize) -> String {
    let mut out: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut cut = max.min(out.len());
    while !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    out
}

/// Parses the lines after the token: `command <old> <new> <ref>` and
/// `pack <path>`. Every other line is ignored, so a hook can't name a
/// repository or principal.
fn parse_request(lines: &[Vec<u8>]) -> Result<HookRequest, String> {
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

fn pkt_len(len: [u8; 4]) -> io::Result<Option<usize>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid pkt-line length");
    let text = std::str::from_utf8(&len).map_err(|_| invalid())?;
    let n = usize::from_str_radix(text, 16).map_err(|_| invalid())?;
    match n {
        0 => Ok(None),
        5..=MAX_PKT_LEN => Ok(Some(n - 4)),
        _ => Err(invalid()),
    }
}

fn encode_pkt(out: &mut Vec<u8>, payload: &[u8]) {
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
fn read_section(r: &mut impl Read) -> io::Result<Vec<Vec<u8>>> {
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

fn write_section(w: &mut impl Write, lines: &[Vec<u8>]) -> io::Result<()> {
    let mut out = Vec::new();
    for line in lines {
        encode_pkt(&mut out, line);
    }
    out.extend_from_slice(b"0000");
    w.write_all(&out)?;
    w.flush()
}

fn trim_newline(mut line: Vec<u8>) -> Vec<u8> {
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    line
}

/// The hook's end of the channel, adopted from the descriptors the parent
/// passed down.
#[derive(Debug)]
pub struct HookChannel {
    reader: BufReader<File>,
    writer: File,
    token: String,
}

impl HookChannel {
    /// Opens the channel named by `OPENXET_HOOK_FD` and reads
    /// `OPENXET_HOOK_TOKEN`.
    ///
    /// # Errors
    ///
    /// An error naming the variable when either is missing or malformed, or
    /// the descriptors can't be opened.
    pub fn from_env() -> io::Result<Self> {
        let missing = |what: &str| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{what} is not set; `openxet proc-receive` runs only as the hook of a push \
                     that openxet serves"
                ),
            )
        };
        let fds = std::env::var(HOOK_FD_ENV).map_err(|_| missing(HOOK_FD_ENV))?;
        let token = std::env::var(HOOK_TOKEN_ENV).map_err(|_| missing(HOOK_TOKEN_ENV))?;
        let (read, write) = fds
            .split_once(',')
            .and_then(|(r, w)| Some((r.parse::<u32>().ok()?, w.parse::<u32>().ok()?)))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{HOOK_FD_ENV} must be two descriptor numbers, `read,write`"),
                )
            })?;
        let reader = File::open(fd_path(read))?;
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(fd_path(write))?;
        Ok(Self {
            reader: BufReader::new(reader),
            writer,
            token,
        })
    }

    /// The token to present.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Sends `lines` as one request and returns the parent's reply lines.
    ///
    /// # Errors
    ///
    /// I/O errors, and [`io::ErrorKind::UnexpectedEof`] when the parent
    /// closed the channel without answering.
    pub fn exchange(&mut self, lines: &[Vec<u8>]) -> io::Result<Vec<Vec<u8>>> {
        write_section(&mut self.writer, lines)?;
        read_section(&mut self.reader)
    }
}

fn fd_path(fd: u32) -> PathBuf {
    PathBuf::from(format!("/dev/fd/{fd}"))
}

/// git's side of the proc-receive protocol: answers the version
/// negotiation (no capabilities, so git sends no push options) and returns
/// the command lines.
///
/// # Errors
///
/// I/O errors, and [`io::ErrorKind::InvalidData`] when git doesn't offer
/// `version=1`.
pub fn negotiate(input: &mut impl Read, output: &mut impl Write) -> io::Result<Vec<Vec<u8>>> {
    let version = read_section(input)?;
    // `version=1`, then optionally a NUL and git's capabilities.
    let offered = version.first().is_some_and(|line| {
        let version = line.split(|b| *b == 0).next().unwrap_or_default();
        version.strip_suffix(b"\n").unwrap_or(version) == b"version=1"
    });
    if !offered {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "git did not offer proc-receive protocol version 1",
        ));
    }
    write_section(output, &[b"version=1\0".to_vec()])?;
    Ok(read_section(input)?.into_iter().map(trim_newline).collect())
}

/// The packs receive-pack is writing for this push: those with the `.keep`
/// file `index-pack --keep` leaves until the push ends, in the quarantine
/// directory when there is one and in `objects/pack` of the repository the
/// hook runs in.
#[must_use]
pub fn new_packs(git_dir: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![git_dir.join("objects/pack")];
    if let Some(quarantine) = std::env::var_os("GIT_QUARANTINE_PATH") {
        dirs.push(PathBuf::from(quarantine).join("pack"));
    }
    let mut packs = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension() == Some(OsStr::new("pack")) && path.with_extension("keep").exists()
            {
                packs.push(std::path::absolute(&path).unwrap_or(path));
            }
        }
    }
    packs.sort();
    packs
}

/// The request lines for `commands` and `packs`.
#[must_use]
pub fn request_lines(token: &str, commands: &[Vec<u8>], packs: &[PathBuf]) -> Vec<Vec<u8>> {
    let mut lines = vec![format!("token {token}").into_bytes()];
    for pack in packs {
        let mut line = b"pack ".to_vec();
        line.extend_from_slice(pack.as_os_str().as_bytes());
        lines.push(line);
    }
    for command in commands {
        let mut line = b"command ".to_vec();
        line.extend_from_slice(command);
        lines.push(line);
    }
    lines
}

/// Relays the parent's reply to git: messages to `messages` (git shows the
/// hook's stderr to the pusher), results as the proc-receive report.
///
/// # Errors
///
/// I/O errors, and an error carrying the parent's reason when it refused
/// the request.
pub fn report(
    reply: &[Vec<u8>],
    output: &mut impl Write,
    messages: &mut impl Write,
) -> io::Result<()> {
    let mut report = Vec::new();
    for line in reply {
        if let Some(text) = line.strip_prefix(b"message ") {
            messages.write_all(text)?;
            messages.write_all(b"\n")?;
        } else if let Some(reason) = line.strip_prefix(b"error ") {
            return Err(io::Error::other(
                String::from_utf8_lossy(reason).into_owned(),
            ));
        } else if line.starts_with(b"ok ") || line.starts_with(b"ng ") {
            report.push(line.clone());
        }
    }
    messages.flush()?;
    write_section(output, &report)
}

/// `openxet proc-receive`: the whole hook, over stdin, stdout and stderr.
/// Returns the process exit code: 0 once git has the report, 1 otherwise,
/// which fails every command of the push.
#[must_use]
pub fn proc_receive_hook() -> u8 {
    match run_hook() {
        Ok(()) => 0,
        Err(e) => {
            let _ = writeln!(io::stderr(), "openxet proc-receive: {e}");
            1
        }
    }
}

fn run_hook() -> io::Result<()> {
    let mut channel = HookChannel::from_env()?;
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let commands = negotiate(&mut input, &mut output)?;
    let packs = new_packs(Path::new("."));
    let request = request_lines(channel.token(), &commands, &packs);
    let reply = channel.exchange(&request)?;
    report(&reply, &mut output, &mut io::stderr().lock())
}

/// Path checks for what the hook reports (ADR 0004 item 7): a pack path
/// must resolve, symlinks and `..` included, inside `root`, the request's
/// snapshot (receive-pack's quarantine lives inside it).
pub(crate) fn confine(path: &Path, root: &Path) -> Option<PathBuf> {
    let resolved = std::fs::canonicalize(path).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    resolved.starts_with(&root).then_some(resolved)
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
