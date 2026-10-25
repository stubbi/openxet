//! The read side of the Git engine (ADR 0004 item 2): the ref
//! advertisement, `ls-refs` answered in-process, the v2 command allowlist,
//! and `fetch` (v2) or the want section (v0/v1) checked for reachability
//! (R22) before `git upload-pack --stateless-rpc` sees it, on a snapshot
//! taken for the request.

use std::path::PathBuf;
use std::process::Stdio;

use bytes::{Bytes, BytesMut};
use futures_core::stream::BoxStream;
use futures_util::StreamExt as _;
use openxet_index::{Oid, RepoId};
use openxet_storage::BodyStream;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;

use crate::interpose::{HeadError, Split, read_head};
use crate::ls_refs::{filter_v2_advertisement, ls_refs, with_openxet_agent};
use crate::pkt::{V2Command, err_pkt, parse_v0_wants, parse_v2_command, quote, quote_at_most};
use crate::{Engine, GitError, SnapshotRepo, git_command, io_err};

/// A response body, streamed block by block; a failure partway ends it with
/// the error, which the transport turns into an aborted response.
pub type ResponseStream = BoxStream<'static, Result<Bytes, std::io::Error>>;

/// A smart Git service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    /// `git-upload-pack`: fetch and clone.
    UploadPack,
    /// `git-receive-pack`: push.
    ReceivePack,
}

impl Service {
    /// The git subcommand: `upload-pack` or `receive-pack`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UploadPack => "upload-pack",
            Self::ReceivePack => "receive-pack",
        }
    }
}

/// How much of git's stderr a failed request logs.
const STDERR_KEPT: u64 = 16 * 1024;

/// The most characters of git's stderr a log line or error carries,
/// escaped.
const STDERR_LOGGED: usize = 2048;

/// The block size of a streamed response.
const READ_BLOCK: usize = 64 * 1024;

impl Engine {
    /// The ref advertisement of `service` for `repo`, as
    /// `info/refs?service=git-{service}` answers it after any HTTP
    /// preamble (`# service=...` and a flush, which smart HTTP adds for
    /// protocol v0 and v1 only).
    ///
    /// For `upload-pack` with protocol 2 it is the capability
    /// advertisement git printed at start-up, filtered to ADR 0004 item 2's
    /// allowlist (`agent`, `ls-refs`, `fetch` without `filter`,
    /// `server-option`, `object-format`), with no snapshot taken. Otherwise
    /// git prints it from a snapshot of `repo`; git serves `receive-pack`
    /// with protocol 0 whatever is asked, as it has no v2 push. Either way
    /// the `agent` is openxet's (`openxet/{version}`), not git's, so the
    /// server's git version isn't disclosed.
    ///
    /// # Errors
    ///
    /// [`GitError::Index`] for an unknown repository, those of
    /// [`Engine::snapshot`], and [`GitError::GitFailed`].
    pub async fn advertise(
        &self,
        repo: RepoId,
        protocol: u8,
        service: Service,
    ) -> Result<Bytes, GitError> {
        if protocol == 2 && service == Service::UploadPack {
            if self.index.repo_by_id(repo).await?.is_none() {
                return Err(openxet_index::IndexError::UnknownRepo { repo }.into());
            }
            return Ok(Bytes::from(filter_v2_advertisement(&self.v2_advertisement)));
        }
        let snapshot = self.snapshot(repo).await?;
        let args = [service.as_str(), "--advertise-refs", "--stateless-rpc", "."];
        let out = git_command(&self.git, &snapshot, Some(protocol))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(io_err("run", &self.git))?;
        if !out.status.success() {
            return Err(GitError::GitFailed {
                git: self.git.clone(),
                command: args.join(" "),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            });
        }
        Ok(Bytes::from(with_openxet_agent(&out.stdout)))
    }

    /// Serves one stateless `upload-pack` request of `repo` for principal
    /// `who` (ADR 0004 item 2). The body is read up to its first flush
    /// before git runs:
    /// - protocol 2: `ls-refs` is answered in-process; `fetch` has its
    ///   wants checked and is forwarded alone (stateless git serves one
    ///   command); any other command is refused, and git never runs.
    /// - protocol 0 and 1: the want lines are checked, then the request is
    ///   forwarded whole.
    ///
    /// A refused want, a refused command or a malformed request is answered
    /// in the stream with an `ERR` packet, which the client reports as
    /// `remote error: ...`.
    ///
    /// # Errors
    ///
    /// [`GitError::Refused`] when the reachability check is over a
    /// concurrency limit: retryable, so smart HTTP answers 429 and SSH
    /// writes [`Refused::pkt`](crate::Refused::pkt). [`GitError::Body`] if the body can't be
    /// read, [`GitError::Objects`] if `ls-refs` can't read the snapshot,
    /// those of [`Engine::snapshot`], and [`GitError::Io`] if git can't be
    /// started.
    pub async fn upload_pack(
        &self,
        repo: RepoId,
        protocol: u8,
        who: &str,
        body: BodyStream,
    ) -> Result<ResponseStream, GitError> {
        let split = match read_head(body).await {
            Ok(split) => split,
            Err(HeadError::Io(e)) => return Err(GitError::Body(e)),
            Err(e) => return Ok(answer(err_pkt(&format!("upload-pack: {e}")))),
        };
        if protocol == 2 {
            let command = match parse_v2_command(&split.head) {
                Ok(command) => command,
                Err(e) => return Ok(answer(err_pkt(&format!("upload-pack: {e}")))),
            };
            match command {
                V2Command::LsRefs(args) => {
                    let snapshot = self.snapshot(repo).await?;
                    let out = crate::blocking(move || ls_refs(&snapshot, &args)).await?;
                    Ok(answer(Bytes::from(out)))
                }
                V2Command::Fetch(fetch) if !fetch.want_refs.is_empty() => Ok(answer(err_pkt(
                    "upload-pack: want-ref is not supported; fetch by object id, as ls-refs lists \
                     them",
                ))),
                V2Command::Fetch(fetch) => {
                    self.checked_fetch(repo, protocol, who, &fetch.wants, split.head, None)
                        .await
                }
                V2Command::Other(name) => Ok(answer(err_pkt(&format!(
                    "upload-pack: command \"{}\" is not supported; this server serves ls-refs \
                     and fetch",
                    quote(name.as_bytes())
                )))),
            }
        } else {
            let wants = match parse_v0_wants(&split.head) {
                Ok(wants) => wants,
                Err(e) => return Ok(answer(err_pkt(&format!("upload-pack: {e}")))),
            };
            let Split { head, extra, rest } = split;
            self.checked_fetch(repo, protocol, who, &wants, head, Some((extra, rest)))
                .await
        }
    }

    /// Checks `wants` on a snapshot of `repo` and, if they pass, runs git
    /// on it with `head` and then, for v0/v1, the rest of the body.
    async fn checked_fetch(
        &self,
        repo: RepoId,
        protocol: u8,
        who: &str,
        wants: &[Oid],
        head: Vec<u8>,
        rest: Option<(Vec<u8>, BodyStream)>,
    ) -> Result<ResponseStream, GitError> {
        let snapshot = self.snapshot(repo).await?;
        match self.reach.check(&snapshot, wants, who).await {
            Ok(()) => {}
            Err(refused) if refused.retryable() => return Err(GitError::Refused(refused)),
            Err(refused) => return Ok(answer(refused.pkt())),
        }
        self.run_upload_pack(snapshot, protocol, head, rest)
    }

    /// Starts `git upload-pack --stateless-rpc` on `snapshot` and streams
    /// its output. The snapshot moves into the stream, so it outlives git.
    fn run_upload_pack(
        &self,
        snapshot: SnapshotRepo,
        protocol: u8,
        head: Vec<u8>,
        rest: Option<(Vec<u8>, BodyStream)>,
    ) -> Result<ResponseStream, GitError> {
        let mut child = git_command(&self.git, &snapshot, Some(protocol))
            .args(["upload-pack", "--stateless-rpc", "."])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(io_err("run", &self.git))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(GitError::Io {
                op: "connect to",
                path: self.git.clone(),
                source: std::io::Error::other("a pipe to git is missing"),
            });
        };
        let feed = AbortOnDrop(tokio::spawn(feed(stdin, head, rest)));
        let stderr = AbortOnDrop(tokio::spawn(async move {
            let mut kept = Vec::new();
            let mut stderr = stderr;
            let _ = (&mut stderr).take(STDERR_KEPT).read_to_end(&mut kept).await;
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            kept
        }));
        let running = Running {
            stdout,
            child,
            _feed: feed,
            stderr,
            git: self.git.clone(),
            snapshot,
            finished: false,
        };
        Ok(futures_util::stream::unfold(running, Running::next).boxed())
    }
}

/// Writes the request to git: `head`, then for v0/v1 the bytes read past
/// it and the rest of the body. A body that fails partway closes git's
/// input, and git fails on the short request.
async fn feed(mut stdin: ChildStdin, head: Vec<u8>, rest: Option<(Vec<u8>, BodyStream)>) {
    if stdin.write_all(&head).await.is_err() {
        return;
    }
    if let Some((extra, mut body)) = rest {
        if stdin.write_all(&extra).await.is_err() {
            return;
        }
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(chunk) => {
                    if stdin.write_all(&chunk).await.is_err() {
                        return;
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "the upload-pack request body failed");
                    return;
                }
            }
        }
    }
    let _ = stdin.shutdown().await;
}

/// A task aborted when its handle is dropped, so an abandoned response
/// stops feeding git.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A running `upload-pack`. Fields drop in order: git is killed (if still
/// running) before its snapshot directory goes.
struct Running {
    stdout: ChildStdout,
    child: Child,
    _feed: AbortOnDrop<()>,
    stderr: AbortOnDrop<Vec<u8>>,
    git: PathBuf,
    snapshot: SnapshotRepo,
    finished: bool,
}

impl Running {
    async fn next(mut self) -> Option<(Result<Bytes, std::io::Error>, Self)> {
        if self.finished {
            return None;
        }
        let mut buf = BytesMut::with_capacity(READ_BLOCK);
        match self.stdout.read_buf(&mut buf).await {
            Ok(0) => {}
            Ok(_) => return Some((Ok(buf.freeze()), self)),
            Err(e) => {
                self.finished = true;
                return Some((Err(e), self));
            }
        }
        self.finished = true;
        let status = self.child.wait().await;
        if matches!(&status, Ok(s) if s.success()) {
            return None;
        }
        let stderr = (&mut self.stderr.0).await.unwrap_or_default();
        let status = status.map_or_else(|e| e.to_string(), |s| s.to_string());
        let stderr = quote_at_most(stderr.trim_ascii(), STDERR_LOGGED);
        tracing::warn!(
            git = %self.git.display(),
            repo = %self.snapshot.repo,
            %status,
            %stderr,
            "git upload-pack failed"
        );
        let err = std::io::Error::other(format!("git upload-pack failed ({status}): {stderr}"));
        Some((Err(err), self))
    }
}

fn answer(bytes: Bytes) -> ResponseStream {
    futures_util::stream::iter([Ok(bytes)]).boxed()
}
