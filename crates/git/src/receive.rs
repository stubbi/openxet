//! `git receive-pack` on a per-request snapshot (ADR 0004 items 1, 3 and
//! 7): openxet streams the client's request into receive-pack, counting the
//! client's pack as it passes, and receive-pack indexes the pack
//! (`--fix-thin`, `receive.fsckObjects`), checks connectivity against the
//! snapshot's committed packs and hands every command to the `proc-receive`
//! hook. The hook relays them to [`Receive::handle`], which re-checks the
//! commands and branch policy, applies the writer reachability rule,
//! reports large plain blobs, publishes the pack and runs the fenced commit
//! transaction, and only then answers `ok` or `ng`.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use futures_core::Stream;
use openxet_auth::UserPrincipal;
use openxet_index::{CommitError, Oid, RefUpdate, RepoId};
use openxet_storage::{BodyStream, PackExt, PackName};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, ChildStdout};
use tokio::sync::oneshot;
use tokio_util::io::ReaderStream;

use crate::hook::{HookCommand, HookContext, HookReply, HookRequest, HookServer, confine};
use crate::publish::{PackFiles, Publication, PublishError, Publisher};
use crate::request::{PackStats, RequestParser};
use crate::thin::{self, Proof, SentObjects, WalkLimits, WriterRefused, analyse, classify_sent};
use crate::{
    Engine, GitConfig, GitError, Reachability, ResponseStream, SnapshotRepo, blocking, git_command,
    io_err,
};

/// The branch that can't be deleted (M2 policy).
const PROTECTED_BRANCH: &str = "refs/heads/main";

/// New blobs of at least this size that aren't LFS pointers are named in
/// the push output with a `git lfs track` suggestion (Review Focus 3). An
/// LFS pointer is under 1 KiB, so any blob this large is file content.
pub const LARGE_BLOB_REPORT_BYTES: u64 = 10 * 1024 * 1024;

/// The largest LFS pointer file: "Pointer files must be less than 1024
/// bytes in size" (git-lfs `docs/spec.md`).
const LFS_POINTER_MAX_BYTES: u64 = 1023;

/// git's reason for the other refs of an atomic push that failed
/// (`builtin/receive-pack.c`, git v2.39.5). A push is one commit
/// transaction, so one refused ref refuses them all.
const ATOMIC_FAILURE: &str = "atomic push failure";

/// How long the hook's handler waits for the end of the client's request
/// after receive-pack has run the hook, by which time it has read the whole
/// pack.
const REQUEST_END_TIMEOUT: Duration = Duration::from_secs(30);

/// How many pushes the writer reachability rule accepted by each path and
/// how many it refused, since the engine started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriterRuleCounts {
    /// Every object made reachable was sent.
    pub sent: u64,
    /// The fast paths placed what wasn't sent.
    pub fast_path: u64,
    /// The full walk was needed.
    pub walk: u64,
    /// Refused.
    pub refused: u64,
}

#[derive(Debug, Default)]
pub(crate) struct Counters {
    sent: AtomicU64,
    fast_path: AtomicU64,
    walk: AtomicU64,
    refused: AtomicU64,
}

impl Counters {
    fn record(&self, outcome: Result<Proof, ()>) {
        let counter = match outcome {
            Ok(Proof::Sent) => &self.sent,
            Ok(Proof::FastPath) => &self.fast_path,
            Ok(Proof::Walk) => &self.walk,
            Err(()) => &self.refused,
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn snapshot(&self) -> WriterRuleCounts {
        WriterRuleCounts {
            sent: self.sent.load(Ordering::Relaxed),
            fast_path: self.fast_path.load(Ordering::Relaxed),
            walk: self.walk.load(Ordering::Relaxed),
            refused: self.refused.load(Ordering::Relaxed),
        }
    }
}

impl Engine {
    /// Serves one `git-receive-pack` request with `--stateless-rpc` (smart
    /// HTTP): `body` is the client's request, the returned stream
    /// receive-pack's answer. `protocol` is the client's `Git-Protocol`
    /// version. Refs move only through the fenced commit transaction, after
    /// the pack is in the bucket (ADR 0004 item 3); `who` is recorded as the
    /// actor in the ref log.
    ///
    /// # Errors
    ///
    /// The errors of [`Engine::snapshot`], and [`GitError::Io`] if git can't
    /// be started.
    pub async fn receive_pack(
        &self,
        repo: RepoId,
        protocol: u8,
        who: &UserPrincipal,
        body: BodyStream,
    ) -> Result<ResponseStream, GitError> {
        let snapshot = Arc::new(self.snapshot(repo).await?);
        let context = HookContext {
            repo,
            actor: who.subject.clone(),
            request: uuid::Uuid::new_v4().simple().to_string(),
        };
        let mut hook = HookServer::new(context)?;
        let mut cmd = git_command(&self.git, &snapshot, Some(protocol));
        cmd.args(["receive-pack", "--stateless-rpc", "."])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        hook.attach(&mut cmd);
        let mut child = cmd.spawn().map_err(io_err("run", &self.git))?;
        // Closes this process's copies of the hook's descriptors, so only
        // the git child holds them.
        drop(cmd);

        let (stats_tx, stats_rx) = oneshot::channel();
        if let Some(stdin) = child.stdin.take() {
            tokio::spawn(feed(body, stdin, stats_tx));
        }
        if let Some(mut stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut text = Vec::new();
                let _ = stderr.read_to_end(&mut text).await;
                if !text.is_empty() {
                    tracing::debug!(stderr = %String::from_utf8_lossy(&text).trim(), "git receive-pack");
                }
            });
        }
        let receive = Receive {
            publisher: Publisher::new(Arc::clone(&self.index), Arc::clone(&self.bucket)),
            config: self.config.clone(),
            snapshot: Arc::clone(&snapshot),
            counters: Arc::clone(&self.counters),
            reach: self.reach.clone(),
        };
        tokio::spawn(hook.serve(move |ctx, request| receive.handle(ctx, request, stats_rx)));

        let Some(stdout) = child.stdout.take() else {
            return Err(GitError::Io {
                op: "read the output of",
                path: self.git.clone(),
                source: io::Error::other("git receive-pack has no stdout"),
            });
        };
        Ok(Box::pin(Response {
            out: ReaderStream::new(stdout),
            _child: child,
            _snapshot: snapshot,
        }))
    }

    /// How many pushes the writer reachability rule accepted by each path,
    /// and how many it refused, since start.
    #[must_use]
    pub fn writer_rule_counts(&self) -> WriterRuleCounts {
        self.counters.snapshot()
    }
}

/// receive-pack's output, holding the child and its snapshot until the
/// stream is dropped: the snapshot must outlive git (crate docs), and
/// dropping the child kills it.
struct Response {
    out: ReaderStream<ChildStdout>,
    _child: Child,
    _snapshot: Arc<SnapshotRepo>,
}

impl Stream for Response {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.out).poll_next(cx)
    }
}

/// Streams `body` into receive-pack, measuring the client's pack on the way
/// (ADR 0004 item 3: openxet is the transport, so it counts the client's
/// bytes), and sends what it measured once the body ends.
async fn feed(
    mut body: BodyStream,
    mut stdin: tokio::process::ChildStdin,
    stats: oneshot::Sender<PackStats>,
) {
    let mut parser = RequestParser::default();
    let mut ok = true;
    while let Some(chunk) = next_chunk(&mut body).await {
        let Ok(chunk) = chunk else {
            ok = false;
            break;
        };
        parser.push(&chunk);
        if stdin.write_all(&chunk).await.is_err() {
            break;
        }
    }
    drop(stdin);
    if ok {
        let _ = stats.send(parser.finish());
    }
}

async fn next_chunk(body: &mut BodyStream) -> Option<io::Result<Bytes>> {
    std::future::poll_fn(|cx| body.as_mut().poll_next(cx)).await
}

/// One push's handler, run when the hook relays the commands.
struct Receive {
    publisher: Publisher,
    config: GitConfig,
    snapshot: Arc<SnapshotRepo>,
    counters: Arc<Counters>,
    reach: Reachability,
}

/// A refusal of the whole push: the refs that caused it with their reasons,
/// the rest fail as an atomic push does.
struct Refusal {
    reasons: Vec<(String, String)>,
    all: Option<String>,
    messages: Vec<String>,
}

impl Refusal {
    fn all(reason: impl Into<String>) -> Self {
        Self {
            reasons: Vec::new(),
            all: Some(reason.into()),
            messages: Vec::new(),
        }
    }

    fn reply(self, commands: &[HookCommand], mut messages: Vec<String>) -> HookReply {
        messages.extend(self.messages);
        let results = commands
            .iter()
            .map(|c| {
                let own = self
                    .reasons
                    .iter()
                    .find(|(name, _)| *name == c.name)
                    .map(|(_, r)| r.clone());
                let reason = own
                    .or_else(|| self.all.clone())
                    .unwrap_or_else(|| ATOMIC_FAILURE.to_owned());
                (c.name.clone(), Err(reason))
            })
            .collect();
        HookReply { messages, results }
    }
}

impl Receive {
    async fn handle(
        self,
        ctx: HookContext,
        request: HookRequest,
        stats: oneshot::Receiver<PackStats>,
    ) -> Option<HookReply> {
        let mut messages = Vec::new();
        match self.decide(&ctx, &request, stats, &mut messages).await {
            Ok(Some(())) => Some(HookReply {
                messages,
                results: request
                    .commands
                    .iter()
                    .map(|c| (c.name.clone(), Ok(())))
                    .collect(),
            }),
            Ok(None) => None,
            Err(refusal) => Some(refusal.reply(&request.commands, messages)),
        }
    }

    /// `Ok(Some(()))` when the push committed, `Ok(None)` when a
    /// fault-injection point stopped it.
    async fn decide(
        &self,
        ctx: &HookContext,
        request: &HookRequest,
        stats: oneshot::Receiver<PackStats>,
        messages: &mut Vec<String>,
    ) -> Result<Option<()>, Refusal> {
        let updates = self.check_policy(&request.commands).await?;
        let stats = tokio::time::timeout(REQUEST_END_TIMEOUT, stats)
            .await
            .ok()
            .and_then(Result::ok)
            .ok_or_else(|| Refusal::all("the push's request ended early; push again"))?;
        let pack = self.new_pack(request.packs.clone(), stats).await?;
        let sent = match &pack {
            Some(files) => {
                let idx = files.path(PackExt::Idx);
                let (bytes, count) = (stats.pack_bytes, stats.header_count.unwrap_or(0));
                blocking(move || classify_sent(&idx, bytes, count))
                    .await
                    .map_err(|e| internal(&e))?
            }
            None => SentObjects::default(),
        };
        let new_tips: Vec<Oid> = updates.iter().filter_map(|u| u.new).collect();
        let max_plain = self.max_plain_blob_bytes();
        let large_bytes = max_plain.map_or(LARGE_BLOB_REPORT_BYTES, |max| {
            LARGE_BLOB_REPORT_BYTES.min(max.saturating_add(1))
        });
        let analysis = match analyse(
            &self.snapshot,
            &new_tips,
            &sent,
            &self.config.thin_bases,
            WalkLimits {
                reach: &self.reach,
                who: &ctx.actor,
            },
            large_bytes,
        )
        .await
        {
            Ok(a) => {
                self.counters.record(Ok(a.proof));
                a
            }
            Err(refused) => {
                self.counters.record(Err(()));
                return Err(writer_refusal(&refused));
            }
        };
        let mut over_limit = Vec::new();
        for blob in &analysis.large_blobs {
            let text = large_blob_message(&blob.path, blob.size);
            match max_plain {
                Some(max) if blob.size > max => over_limit.push(format!(
                    "{text} This server refuses files over {} outside Git LFS.",
                    human_size(max)
                )),
                _ => messages.push(text),
            }
        }
        if !over_limit.is_empty() {
            let reason = over_limit[0].clone();
            let mut refusal = Refusal::all(reason);
            refusal.messages = over_limit;
            return Err(refusal);
        }

        let publication = Publication {
            repo: ctx.repo,
            pack: pack.as_ref(),
            updates: &updates,
            full_repack_gen: self.snapshot.full_repack_gen,
            actor: &ctx.actor,
            request: &ctx.request,
        };
        match self.publisher.publish(publication).await {
            Ok(_) => Ok(Some(())),
            #[cfg(feature = "fault-injection")]
            Err(PublishError::Injected(point)) => {
                tracing::warn!(point, "fault injected into a push");
                Ok(None)
            }
            Err(PublishError::Commit(CommitError::RefMismatch(name))) => Err(Refusal {
                reasons: vec![(name, "fetch first".to_owned())],
                all: None,
                messages: Vec::new(),
            }),
            Err(PublishError::Commit(CommitError::Fenced)) => Err(Refusal::all(
                "the repository was repacked while this push ran; push again",
            )),
            Err(PublishError::Commit(CommitError::PackNotPending)) => {
                Err(Refusal::all("the push took too long to commit; push again"))
            }
            Err(e) => Err(internal(&e)),
        }
    }

    /// Re-checks every command against the snapshot (ADR 0004 item 3):
    /// valid ref names, each ref once, the client's old value equal to the
    /// snapshot's (a forced push is a non-fast-forward on the wire that
    /// only the client can tell apart, so the old value is what fences a
    /// stale update), no deletion of `main`, and branches pointing at
    /// commits.
    async fn check_policy(&self, commands: &[HookCommand]) -> Result<Vec<RefUpdate>, Refusal> {
        let mut reasons = Vec::new();
        let mut seen = HashSet::new();
        for c in commands {
            let current = self
                .snapshot
                .refs
                .iter()
                .find(|(name, _)| *name == c.name)
                .map(|(_, oid)| *oid);
            let reason = if !valid_ref_name(&c.name) {
                Some("funny refname")
            } else if !seen.insert(c.name.as_str()) {
                Some("the ref is updated twice in one push")
            } else if c.old.is_none() && c.new.is_none() {
                Some("nothing to update")
            } else if c.old != current {
                Some("fetch first")
            } else if c.new.is_none() && c.name == PROTECTED_BRANCH {
                Some("deletion of the current branch prohibited")
            } else {
                None
            };
            if let Some(reason) = reason {
                reasons.push((c.name.clone(), reason.to_owned()));
            }
        }
        let branch_tips: Vec<(String, Oid)> = commands
            .iter()
            .filter(|c| c.name.starts_with("refs/heads/"))
            .filter_map(|c| Some((c.name.clone(), c.new?)))
            .collect();
        if reasons.is_empty() && !branch_tips.is_empty() {
            let kinds = self
                .object_kinds(branch_tips.iter().map(|(_, o)| *o))
                .await?;
            for ((name, _), kind) in branch_tips.iter().zip(kinds) {
                match kind.as_deref() {
                    Some("commit") => {}
                    Some(_) => reasons.push((
                        name.clone(),
                        "trying to write non-commit object to branch".to_owned(),
                    )),
                    None => reasons.push((name.clone(), thin::MISSING_OBJECTS.to_owned())),
                }
            }
        }
        if reasons.is_empty() {
            Ok(commands
                .iter()
                .map(|c| RefUpdate {
                    name: c.name.clone(),
                    old: c.old,
                    new: c.new,
                })
                .collect())
        } else {
            Err(Refusal {
                reasons,
                all: None,
                messages: Vec::new(),
            })
        }
    }

    async fn object_kinds(
        &self,
        oids: impl Iterator<Item = Oid>,
    ) -> Result<Vec<Option<String>>, Refusal> {
        let input = thin::oid_lines("", oids);
        let out = thin::run(
            &self.snapshot,
            &["cat-file", "--batch-check=%(objecttype)"],
            input.into_bytes(),
        )
        .await
        .map_err(|e| internal(&e))?
        .map_err(|stderr| internal(&stderr))?;
        Ok(String::from_utf8_lossy(&out)
            .lines()
            .map(|l| (!l.ends_with(" missing")).then(|| l.to_owned()))
            .collect())
    }

    /// `git.max_plain_blob_bytes`, floored below at the largest LFS
    /// pointer, so that no limit can refuse a pointer: "Pointer files must
    /// be less than 1024 bytes in size" (git-lfs `docs/spec.md`).
    fn max_plain_blob_bytes(&self) -> Option<u64> {
        self.config
            .max_plain_blob_bytes
            .map(|max| max.max(LFS_POINTER_MAX_BYTES))
    }

    /// The push's own pack: the one pack in the snapshot's `objects/pack`
    /// that holds the `.keep` file `index-pack --keep` leaves until the push
    /// ends and isn't one of the snapshot's committed packs. receive-pack
    /// migrates its quarantine there before it runs the hook, and keeps
    /// every pushed pack (`receive.unpackLimit=1`), so a client pack with
    /// objects must have exactly one. openxet finds it itself; each path the
    /// hook reported must still resolve, symlinks and `..` included, inside
    /// the snapshot and name that pack (ADR 0004 item 7), as defence in
    /// depth. A pushed pack named like a committed one holds exactly the
    /// same objects (a pack is named by its object set), as when a push
    /// whose answer was lost is sent again: then nothing new is published,
    /// and the rule treats every object as unsent, which only asks more.
    async fn new_pack(
        &self,
        reported: Vec<PathBuf>,
        stats: PackStats,
    ) -> Result<Option<PackFiles>, Refusal> {
        let root = self.snapshot.dir.path().to_path_buf();
        let committed: HashSet<String> =
            self.snapshot.packs.iter().map(|p| p.name.clone()).collect();
        let (mut found, again, all_named) = blocking(move || {
            let (found, again) = kept_packs(&root.join("objects/pack"), &committed)?;
            let ours: Vec<PathBuf> = found
                .iter()
                .chain(&again)
                .filter_map(|f| std::fs::canonicalize(f.path(PackExt::Pack)).ok())
                .collect();
            let all_named = reported
                .iter()
                .all(|p| confine(p, &root).is_some_and(|c| ours.contains(&c)));
            Ok((found, !again.is_empty(), all_named))
        })
        .await
        .map_err(|e| internal(&e))?;
        if !all_named {
            return Err(Refusal::all("the hook reported a pack outside this push"));
        }
        let expected = stats.header_count.is_some_and(|n| n > 0);
        match (found.len(), expected) {
            (0, false) => Ok(None),
            (0, true) if again => Ok(None),
            (1, true) => Ok(found.pop()),
            _ => Err(Refusal::all(
                "the pushed pack doesn't match what receive-pack wrote; push again",
            )),
        }
    }
}

/// The packs in `dir` with a `.keep` file: those not in `committed`, and
/// those named like a committed pack.
fn kept_packs(
    dir: &Path,
    committed: &HashSet<String>,
) -> Result<(Vec<PackFiles>, Vec<PackFiles>), GitError> {
    let (mut found, mut again) = (Vec::new(), Vec::new());
    for entry in std::fs::read_dir(dir).map_err(io_err("list", dir))? {
        let path = entry.map_err(io_err("list", dir))?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".keep"))
            .and_then(|n| PackName::parse(n).ok());
        if let Some(name) = name
            && path.with_extension("pack").is_file()
        {
            let list = if committed.contains(name.as_str()) {
                &mut again
            } else {
                &mut found
            };
            list.push(PackFiles {
                dir: dir.to_path_buf(),
                name,
            });
        }
    }
    Ok((found, again))
}

fn internal(e: &dyn std::fmt::Display) -> Refusal {
    tracing::warn!(error = %e, "a push failed inside openxet");
    Refusal::all(
        "the server failed to complete this push; push again, and report it to the server's \
         operator if it persists",
    )
}

fn writer_refusal(refused: &WriterRefused) -> Refusal {
    if let WriterRefused::Failed(e) = refused {
        tracing::warn!(error = %e, "checking a push's objects failed");
    }
    let mut refusal = Refusal::all(refused.reason());
    if let Some(hint) = refused.hint() {
        refusal.messages.push(hint.to_owned());
    }
    refusal
}

fn large_blob_message(path: &str, size: u64) -> String {
    format!(
        "openxet: {path} is {} and stored as a plain Git object, not with Git LFS. Large files in \
         Git history slow down every clone; track them with: git lfs track \"{path}\"",
        human_size(size)
    )
}

/// A size in MiB with one decimal, in integer arithmetic.
fn human_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    let tenths = (u128::from(bytes) * 10 + u128::from(MIB) / 2) / u128::from(MIB);
    format!("{}.{} MiB", tenths / 10, tenths % 10)
}

/// A ref name git's `check-ref-format` accepts, under `refs/`
/// (git-check-ref-format(1), rules 1 to 10).
fn valid_ref_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("refs/") else {
        return false;
    };
    !rest.is_empty()
        && !name.ends_with('/')
        && !name.ends_with('.')
        && !name.contains("..")
        && !name.contains("//")
        && !name.contains("@{")
        && !name.bytes().any(|b| {
            b < 0x20
                || b == 0x7f
                || matches!(b, b' ' | b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b'\\')
        })
        && name
            .split('/')
            .all(|c| !c.is_empty() && !c.starts_with('.') && !has_lock_suffix(c))
}

/// git refuses a component ending in `.lock` exactly, case-sensitively
/// (`refs.c`, `check_refname_component`).
fn has_lock_suffix(component: &str) -> bool {
    component.as_bytes().ends_with(b".lock")
}

#[cfg(test)]
mod tests {
    use super::{human_size, valid_ref_name};

    #[test]
    fn ref_names() {
        for good in [
            "refs/heads/main",
            "refs/tags/v1.0",
            "refs/heads/feature/x-1",
        ] {
            assert!(valid_ref_name(good), "{good}");
        }
        for bad in [
            "HEAD",
            "refs/",
            "refs/heads/",
            "refs/heads/a..b",
            "refs/heads/.hidden",
            "refs/heads/x.lock",
            "refs/heads/a b",
            "refs/heads/a@{1}",
            "refs/heads/a\\b",
            "refs//heads",
            "refs/heads/x.",
        ] {
            assert!(!valid_ref_name(bad), "{bad}");
        }
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(20 * 1024 * 1024), "20.0 MiB");
        assert_eq!(human_size(10_485_760 + 524_288), "10.5 MiB");
    }
}
