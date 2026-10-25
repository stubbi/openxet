//! The parent's end of the hook channel: the pipes, the single-use token
//! and the one request it serves (ADR 0004 item 7).

use std::io;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};

use command_fds::CommandFdExt as _;
use subtle::ConstantTimeEq as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::unix::pipe;

use super::{
    HOOK_FD_ENV, HOOK_TOKEN_ENV, HookContext, HookReply, HookRequest, MAX_PKT_LEN,
    MAX_REQUEST_LINES, encode_pkt, parse_request, pkt_len,
};
use crate::GitError;

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
pub struct HookServer {
    context: HookContext,
    token: String,
    from_hook: pipe::Receiver,
    to_hook: pipe::Sender,
    /// The hook's ends: what it reads, what it writes. Moved into the git
    /// child by [`HookServer::attach`].
    child: Option<(OwnedFd, OwnedFd)>,
}

impl std::fmt::Debug for HookServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookServer")
            .field("context", &self.context)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
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
        check_request(&self.token, lines)
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

/// The parent's check of a request's lines: the first presents the token,
/// compared in constant time; the rest are parsed.
pub(crate) fn check_request(token: &str, lines: &[Vec<u8>]) -> Result<HookRequest, String> {
    let presented = lines.first().and_then(|l| l.strip_prefix(b"token "));
    let valid = presented.is_some_and(|t| bool::from(t.ct_eq(token.as_bytes())));
    if !valid {
        return Err("the hook token is missing or wrong".to_owned());
    }
    parse_request(&lines[1..])
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
    use super::{HookContext, HookServer};

    #[tokio::test]
    async fn debug_redacts_the_token() {
        let server = HookServer::new(HookContext {
            repo: openxet_index::RepoId::generate(),
            actor: "pusher".to_owned(),
            request: "req".to_owned(),
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let shown = format!("{server:?}");
        assert!(shown.contains("<redacted>"), "{shown}");
        assert!(!shown.contains(&server.token), "{shown}");
    }
}
