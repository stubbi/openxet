//! Hub writes (ADR 0004 item 6): Hub commits, annotated tags and ref
//! creates, updates and deletes. Each attempt takes a fresh snapshot. New
//! objects go into one pack in the snapshot's quarantine directory, which
//! `git index-pack --strict` indexes (the counterpart of
//! `receive.fsckObjects`); `git rev-list --objects <new> --not <tips>`, with
//! the quarantine as object directory and the snapshot as its only
//! alternate, checks it is connected; then the writer reachability rule
//! runs as for a push, and the pack is published through the fenced commit
//! transaction ([`Publisher`]). A ref write without new objects checks its
//! target as R22 checks a want and commits with no pack. An attempt that
//! loses its transaction to a concurrent writer or to the repack fence, or
//! whose pack row the pack sweep refuses while it deletes files of the same
//! name, runs again, at most [`RETRIES`] times, each with a new snapshot
//! and a new pack row (ADR 0004 item 3).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use gix::objs::Kind;
use openxet_auth::UserPrincipal;
use openxet_index::{CommitError, IndexError, Oid, RefUpdate, RepoId};
use openxet_storage::PackExt;

use super::pack::{link_pack, run_quarantined, write_pack};
use super::read::{oid_of, open_snapshot};
use super::tree::{Objects, build_commit, components, op_path, signature};
use super::{HubCommit, Ident, TagInfo};
use crate::publish::{PackFiles, Publication, PublishError, Publisher};
use crate::reach::{kind, to_gix};
use crate::receive::valid_ref_name;
use crate::snapshot::HEAD_REF;
use crate::thin::{WalkLimits, WriterRefused, check_writer_rule, classify_sent, oid_lines};
use crate::{Engine, GitError, SnapshotRepo, blocking};

/// How many times a Hub write runs again after losing its commit
/// transaction (ADR 0004 item 3).
const RETRIES: usize = 3;

/// How long a Hub write waits before running again after the pack sweep
/// refused its pack row ([`IndexError::PackDeleting`]).
const PACK_DELETING_PAUSE: std::time::Duration = std::time::Duration::from_millis(100);

/// With the `fault-injection` feature, a Hub write by this actor leaves
/// every blob out of its pack, as a writer that names objects without
/// sending them would.
#[cfg(feature = "fault-injection")]
pub const FAULT_OMIT_BLOBS_ACTOR: &str = "openxet-test/hub-omit-blobs";

/// Held while a Hub commit to one branch runs on this node.
type BranchLock = tokio::sync::Mutex<()>;

/// The Hub write state of one engine.
#[derive(Debug, Default)]
pub(crate) struct HubWrites {
    /// One lock per repository and branch: Hub commits to one branch on one
    /// node take turns instead of racing each other for the ref, so they all
    /// land (Review Focus 2); across nodes the retries settle the race. A
    /// commit waits for its turn at most `hub.commit_wait`
    /// ([`GitConfig::hub_commit_wait`](crate::GitConfig::hub_commit_wait)).
    locks: Mutex<HashMap<(RepoId, String), Weak<BranchLock>>>,
    #[cfg(feature = "fault-injection")]
    hold: Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
}

impl HubWrites {
    fn lock(&self, repo: RepoId, branch: &str) -> Arc<BranchLock> {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        let key = (repo, branch.to_owned());
        if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
            return lock;
        }
        locks.retain(|_, l| l.strong_count() > 0);
        let lock = Arc::new(BranchLock::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }
}

/// A Hub write held between its snapshot and its commit transaction, for
/// the fault-injection tests (ADR 0004, `publish_then_cas_survives_faults`).
#[cfg(feature = "fault-injection")]
#[derive(Debug)]
pub struct HubWriteHold {
    reached: tokio::sync::oneshot::Receiver<()>,
    release: tokio::sync::oneshot::Sender<()>,
}

#[cfg(feature = "fault-injection")]
impl HubWriteHold {
    /// Waits until the held write reaches its commit transaction; `false`
    /// if the engine is gone.
    pub async fn reached(&mut self) -> bool {
        (&mut self.reached).await.is_ok()
    }

    /// Lets the held write run its commit transaction.
    pub fn release(self) {
        let _ = self.release.send(());
    }
}

/// How one attempt ended.
enum Outcome {
    Committed,
    /// The transaction lost to a concurrent writer or the repack fence.
    Retry,
}

impl Engine {
    /// Commits `req` to its branch as `who` (ADR 0004 item 6, ruling M2-R7):
    /// on a fresh snapshot, builds the commit on the branch tip (which must
    /// equal `req.parent` when given), packs, indexes and checks its new
    /// objects, applies the writer reachability rule and publishes through
    /// the fenced commit transaction. Retries up to three times, each from a
    /// fresh snapshot, when another writer moved the branch or a full
    /// repack published meanwhile. Commits to one branch on this node run
    /// one at a time; one that waits longer than `hub.commit_wait` for its
    /// turn is refused as [`GitError::Busy`].
    ///
    /// # Errors
    ///
    /// [`GitError::InvalidHubWrite`] for a bad branch name, path or
    /// operation; [`GitError::EntryNotFound`] for a delete of a missing
    /// file or folder; [`GitError::RevisionNotFound`] for a missing branch
    /// in a repository that has refs; [`GitError::ParentMismatch`];
    /// [`GitError::MissingObjects`] if the rule refuses;
    /// [`GitError::Busy`] if its turn doesn't come within `hub.commit_wait`;
    /// [`GitError::Contended`] when every attempt lost; and the errors of
    /// [`Engine::snapshot`] and of the publish.
    pub async fn hub_commit(
        &self,
        repo: RepoId,
        who: &UserPrincipal,
        req: HubCommit<'_>,
    ) -> Result<Oid, GitError> {
        let branch = branch_ref(req.branch)?;
        for op in &req.ops {
            components(op_path(op))?;
        }
        let message = commit_message(req.summary, req.description);
        let author = Ident::author(who);
        let request = new_request();
        // One time for every attempt: a retry rebuilds the same objects on
        // the same tip.
        let time = now();
        let lock = self.hub.lock(repo, &branch);
        let _turn = tokio::time::timeout(self.config.hub_commit_wait, lock.lock())
            .await
            .map_err(|_| GitError::Busy)?;
        for _ in 0..=RETRIES {
            let snapshot = self.snapshot(repo).await?;
            let tip = ref_target(&snapshot, &branch);
            match (tip, req.parent) {
                (actual, Some(expected)) if actual != Some(expected) => {
                    return Err(GitError::ParentMismatch {
                        branch,
                        expected,
                        actual,
                    });
                }
                (None, _) if !snapshot.refs.is_empty() => {
                    return Err(GitError::RevisionNotFound {
                        revision: req.branch.to_owned(),
                    });
                }
                _ => {}
            }
            let dir = snapshot.dir.path().to_path_buf();
            let (ops, author, message) = (req.ops.clone(), author.clone(), message.clone());
            let (commit, objects) = blocking(move || {
                build_commit(&open_snapshot(&dir)?, tip, &ops, &author, &message, time)
            })
            .await?;
            let update = RefUpdate {
                name: branch.clone(),
                old: tip,
                new: Some(commit),
            };
            match self
                .publish_objects(&snapshot, objects, &update, &who.subject, &request)
                .await?
            {
                Outcome::Committed => return Ok(commit),
                Outcome::Retry => {}
            }
        }
        Err(GitError::Contended { name: branch })
    }

    /// Creates, moves or deletes a ref as `who` (ADR 0004 item 6), through
    /// the fenced commit transaction: `update.old` must be the ref's target
    /// in a fresh snapshot (`None`: it must not exist), and a new target
    /// must pass R22 from the snapshot's refs as a fetch want does, so it is
    /// in the committed set and reachable; a branch must point at a commit.
    /// With `tag`, an annotated tag object pointing at the target is built,
    /// packed and published like a Hub commit, and the ref points at it.
    /// Retries up to three times, each from a fresh snapshot, after losing
    /// to the repack fence or a concurrent writer.
    ///
    /// # Errors
    ///
    /// [`GitError::InvalidHubWrite`] for a bad ref name, a delete of `main`,
    /// a command that changes nothing, a tag outside `refs/tags/` or a
    /// branch at a non-commit; [`GitError::RefConflict`] if the ref isn't at
    /// `update.old`; [`GitError::RevisionNotFound`] for a target R22
    /// refuses; [`GitError::Busy`] over R22's concurrency limits;
    /// [`GitError::Contended`] when every attempt lost; and the errors of
    /// [`Engine::snapshot`] and of the publish.
    pub async fn hub_ref_write(
        &self,
        repo: RepoId,
        who: &UserPrincipal,
        update: RefUpdate,
        tag: Option<TagInfo>,
    ) -> Result<(), GitError> {
        check_ref_command(&update, tag.as_ref())?;
        let request = new_request();
        // One time for every attempt: a retry builds the same tag object,
        // so its pack has the same name as the lost attempt's.
        let time = now();
        for _ in 0..=RETRIES {
            let snapshot = self.snapshot(repo).await?;
            if ref_target(&snapshot, &update.name) != update.old {
                return Err(GitError::RefConflict {
                    name: update.name.clone(),
                });
            }
            let outcome = match update.new {
                None => {
                    self.publish(&snapshot, None, &update, &who.subject, &request)
                        .await?
                }
                Some(target) => {
                    let tag = tag.as_ref().map(|t| (t, time));
                    self.ref_write_to(&snapshot, &update, target, tag, who, &request)
                        .await?
                }
            };
            if let Outcome::Committed = outcome {
                return Ok(());
            }
        }
        Err(GitError::Contended { name: update.name })
    }

    /// One attempt of a ref create or update to `target`.
    async fn ref_write_to(
        &self,
        snapshot: &SnapshotRepo,
        update: &RefUpdate,
        target: Oid,
        tag: Option<(&TagInfo, i64)>,
        who: &UserPrincipal,
        request: &str,
    ) -> Result<Outcome, GitError> {
        let not_found = || GitError::RevisionNotFound {
            revision: target.to_string(),
        };
        // The writer reachability rule for a writer without a pack: the
        // target is checked exactly like an R22 want (ADR 0004 item 3).
        if let Err(refused) = self.reach.check(snapshot, &[target], &who.subject).await {
            return Err(if refused.retryable() {
                GitError::Busy
            } else {
                not_found()
            });
        }
        let dir = snapshot.dir.path().to_path_buf();
        let target_kind =
            blocking(move || kind(&open_snapshot(&dir)?, target).map_err(GitError::Objects))
                .await?
                .ok_or_else(not_found)?;
        if update.name.starts_with("refs/heads/") && target_kind != Kind::Commit {
            return Err(GitError::InvalidHubWrite(format!(
                "{} must point at a commit, and {target} is a {target_kind}; name a commit or a \
                 branch",
                update.name
            )));
        }
        let Some((tag, time)) = tag else {
            return self
                .publish(snapshot, None, update, &who.subject, request)
                .await;
        };
        let mut objects = Objects::default();
        let name = update
            .name
            .strip_prefix("refs/tags/")
            .unwrap_or(&update.name);
        let id = objects.write(&gix::objs::Tag {
            target: to_gix(target),
            target_kind,
            name: name.into(),
            tagger: Some(signature(&tag.tagger, time)),
            message: with_newline(&tag.message).into(),
            signature: None,
        })?;
        let update = RefUpdate {
            new: Some(oid_of(id)?),
            ..update.clone()
        };
        self.publish_objects(snapshot, objects, &update, &who.subject, request)
            .await
    }

    /// Packs `objects` in the snapshot's quarantine, indexes and checks the
    /// pack, applies the writer reachability rule to `update`'s new target,
    /// and publishes.
    async fn publish_objects(
        &self,
        snapshot: &SnapshotRepo,
        objects: Objects,
        update: &RefUpdate,
        actor: &str,
        request: &str,
    ) -> Result<Outcome, GitError> {
        #[cfg_attr(not(feature = "fault-injection"), allow(unused_mut))]
        let mut objects = objects.into_vec();
        #[cfg(feature = "fault-injection")]
        if actor == FAULT_OMIT_BLOBS_ACTOR {
            objects.retain(|o| o.kind != Kind::Blob);
        }
        let new_tips: Vec<Oid> = update.new.into_iter().collect();
        let quarantine = snapshot.dir.path().join("objects/quarantine");
        let pack_dir = quarantine.join("pack");
        let (files, count, bytes) = blocking(move || write_pack(&pack_dir, &objects)).await?;
        let pack = files.path(PackExt::Pack);
        let indexed = run_quarantined(
            snapshot,
            &quarantine,
            &[
                OsStr::new("index-pack"),
                OsStr::new("--strict"),
                pack.as_os_str(),
            ],
            Vec::new(),
        )
        .await?;
        if let Err(stderr) = indexed {
            return Err(GitError::InvalidHubWrite(fsck_refusal(&stderr)));
        }
        let mut input = oid_lines("", &new_tips);
        input.push_str(&oid_lines("^", snapshot.refs.iter().map(|(_, oid)| *oid)));
        let connected = run_quarantined(
            snapshot,
            &quarantine,
            &["rev-list", "--objects", "--quiet", "--stdin"].map(OsStr::new),
            input.into_bytes(),
        )
        .await?;
        if connected.is_err() {
            // git's text names object ids from the request: none in the log.
            tracing::warn!("a Hub write's objects are not connected in its snapshot");
            return Err(GitError::MissingObjects);
        }

        let linked = files.clone();
        let objects_dir = snapshot.dir.path().join("objects/pack");
        let sent = blocking(move || {
            link_pack(&linked, &objects_dir)?;
            classify_sent(&linked.path(PackExt::Idx), bytes, count)
        })
        .await?;
        let limits = WalkLimits {
            reach: &self.reach,
            who: actor,
        };
        check_writer_rule(snapshot, &new_tips, &sent, &self.config.thin_bases, limits)
            .await
            .map_err(writer_error)?;
        self.publish(snapshot, Some(&files), update, actor, request)
            .await
    }

    /// Runs the fenced commit transaction (ADR 0004 item 3) for `update`
    /// with `pack`, after uploading it.
    async fn publish(
        &self,
        snapshot: &SnapshotRepo,
        pack: Option<&PackFiles>,
        update: &RefUpdate,
        actor: &str,
        request: &str,
    ) -> Result<Outcome, GitError> {
        #[cfg(feature = "fault-injection")]
        self.hold_point().await;
        let publisher = Publisher::new(Arc::clone(&self.index), Arc::clone(&self.bucket));
        let publication = Publication {
            repo: snapshot.repo,
            pack,
            updates: std::slice::from_ref(update),
            full_repack_gen: snapshot.full_repack_gen,
            actor,
            request,
        };
        match publisher.publish(publication).await {
            Ok(_) => Ok(Outcome::Committed),
            Err(PublishError::Commit(
                CommitError::Fenced | CommitError::RefMismatch(_) | CommitError::PackNotPending,
            )) => Ok(Outcome::Retry),
            // The pack sweep is deleting files of this pack's name (a lost
            // attempt's, ADR 0004 item 5): a retry rebuilds the same pack,
            // so give the sweep a moment to finish first.
            Err(PublishError::Index(IndexError::PackDeleting { .. })) => {
                tokio::time::sleep(PACK_DELETING_PAUSE).await;
                Ok(Outcome::Retry)
            }
            Err(e) => Err(publish_error(e)),
        }
    }

    /// Holds the next Hub write between its snapshot and its commit
    /// transaction, after the writer reachability rule, until the returned
    /// hold is released.
    #[cfg(feature = "fault-injection")]
    #[must_use]
    pub fn hold_next_hub_write(&self) -> HubWriteHold {
        let (reached_tx, reached) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        *self.hub.hold.lock().unwrap_or_else(PoisonError::into_inner) =
            Some((reached_tx, release_rx));
        HubWriteHold { reached, release }
    }

    #[cfg(feature = "fault-injection")]
    async fn hold_point(&self) {
        let held = self
            .hub
            .hold
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some((reached, release)) = held {
            let _ = reached.send(());
            let _ = release.await;
        }
    }
}

/// The full ref name of a Hub commit's branch: `refs/heads/{branch}`, or
/// the name itself if it is a full ref name.
fn branch_ref(branch: &str) -> Result<String, GitError> {
    let name = if branch.starts_with("refs/") {
        branch.to_owned()
    } else {
        format!("refs/heads/{branch}")
    };
    if valid_ref_name(&name) {
        Ok(name)
    } else {
        Err(GitError::InvalidHubWrite(format!(
            "{branch:?} is not a valid branch name (git-check-ref-format(1))"
        )))
    }
}

/// Checks a Hub ref command before any snapshot.
fn check_ref_command(update: &RefUpdate, tag: Option<&TagInfo>) -> Result<(), GitError> {
    let invalid = |why: String| Err(GitError::InvalidHubWrite(why));
    if !valid_ref_name(&update.name) {
        return invalid(format!(
            "{:?} is not a valid ref name (git-check-ref-format(1))",
            update.name
        ));
    }
    if update.old.is_none() && update.new.is_none() {
        return invalid(format!("nothing to change for {}", update.name));
    }
    if update.new.is_none() && update.name == HEAD_REF {
        return invalid(format!(
            "{HEAD_REF} is the default branch and can't be deleted"
        ));
    }
    if tag.is_some() && (update.new.is_none() || !update.name.starts_with("refs/tags/")) {
        return invalid(format!(
            "an annotated tag needs a target and a name under refs/tags/, not {}",
            update.name
        ));
    }
    Ok(())
}

fn ref_target(snapshot: &SnapshotRepo, name: &str) -> Option<Oid> {
    snapshot
        .refs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, oid)| *oid)
}

/// The message of a Hub commit (ruling M2-R7): the summary, then a blank
/// line and the description when there is one, newline-terminated as git
/// writes messages.
fn commit_message(summary: &str, description: &str) -> String {
    let description = description.trim_end_matches('\n');
    if description.is_empty() {
        with_newline(summary)
    } else {
        format!("{}\n\n{description}\n", summary.trim_end_matches('\n'))
    }
}

fn with_newline(text: &str) -> String {
    format!("{}\n", text.trim_end_matches('\n'))
}

fn new_request() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The refusal of a Hub write `index-pack --strict` rejected: a fixed text
/// with git's fsck message id (`hasDotgit`, `badTimezone`, ...) when its
/// output names one, never git's text, which quotes the request's names
/// and object ids.
fn fsck_refusal(stderr: &str) -> String {
    const TEXT: &str = "git's object checks refused this write; check its paths and names";
    match fsck_msg_id(stderr) {
        Some(id) => format!("{TEXT} (git fsck: {id})"),
        None => TEXT.to_owned(),
    }
}

/// The first fsck message id in git's output: the camelCase word after
/// `object <id>: ` (fsck.c `fsck_vreport`), as in `error: object 1f…:
/// hasDotgit: contains '.git'`.
fn fsck_msg_id(stderr: &str) -> Option<&str> {
    stderr.lines().find_map(|line| {
        let (_, rest) = line.split_once("object ")?;
        let (oid, rest) = rest.split_once(": ")?;
        if oid.is_empty() || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let (id, _) = rest.split_once(':')?;
        let ok = id.len() <= 64
            && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
            && id.bytes().all(|b| b.is_ascii_alphanumeric());
        ok.then_some(id)
    })
}

fn writer_error(refused: WriterRefused) -> GitError {
    match refused {
        WriterRefused::Busy => GitError::Busy,
        WriterRefused::Failed(e) => e,
        WriterRefused::Unreachable { .. } | WriterRefused::OverBudget { .. } => {
            GitError::MissingObjects
        }
    }
}

fn publish_error(e: PublishError) -> GitError {
    match e {
        PublishError::Commit(CommitError::Database(e)) | PublishError::Index(e) => {
            GitError::Index(e)
        }
        PublishError::Commit(e) => GitError::Corrupt(format!("unexpected commit failure: {e}")),
        PublishError::Storage(e) => GitError::Storage(e),
        PublishError::Read { path, source } => GitError::Io {
            op: "read the Hub write's pack",
            path,
            source,
        },
        #[cfg(feature = "fault-injection")]
        PublishError::Injected(point) => GitError::Injected(point),
    }
}

#[cfg(test)]
mod tests {
    use openxet_index::{Oid, RefUpdate};

    use super::{branch_ref, check_ref_command, commit_message, fsck_msg_id, fsck_refusal};

    #[test]
    fn fsck_refusals_carry_only_the_message_id() {
        let stderr = "error: object 0123abcd: hasDotgit: contains '.git' at secret/path\n\
                      fatal: fsck error in packed object";
        assert_eq!(fsck_msg_id(stderr), Some("hasDotgit"));
        let text = fsck_refusal(stderr);
        assert!(text.ends_with("(git fsck: hasDotgit)"), "{text}");
        assert!(
            !text.contains("secret") && !text.contains("0123abcd"),
            "{text}"
        );
        assert_eq!(fsck_msg_id("fatal: pack has bad object at offset 12"), None);
        assert_eq!(fsck_msg_id("error: object 12: Bad Id: x"), None);
    }

    #[test]
    fn messages() {
        assert_eq!(commit_message("s", ""), "s\n");
        assert_eq!(commit_message("s", "d\n\n"), "s\n\nd\n");
        assert_eq!(commit_message("s\n", "a\nb"), "s\n\na\nb\n");
    }

    #[test]
    fn names() {
        assert_eq!(branch_ref("main").ok().as_deref(), Some("refs/heads/main"));
        assert_eq!(branch_ref("refs/pr/1").ok().as_deref(), Some("refs/pr/1"));
        assert!(branch_ref("a b").is_err());
        let set = |name: &str, old, new| RefUpdate {
            name: name.to_owned(),
            old,
            new,
        };
        let some = Some(Oid([1; 20]));
        assert!(check_ref_command(&set("refs/heads/x", None, some), None).is_ok());
        assert!(check_ref_command(&set("refs/heads/x", None, None), None).is_err());
        assert!(check_ref_command(&set("refs/heads/main", some, None), None).is_err());
        assert!(check_ref_command(&set("x", None, some), None).is_err());
    }
}
