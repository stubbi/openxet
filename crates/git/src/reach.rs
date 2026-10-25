//! R22 for readers (ADR 0004 item 2): every want of a fetch must be
//! reachable from the refs of the request's snapshot before git sees it,
//! because git's v2 `parse_want` only checks that the object exists.
//!
//! A want equal to a ref tip, or to an object a tag chain from a ref tip
//! points at, passes without walking: that is every normal clone and fetch.
//! The remaining commit wants share one gix walk over commits (commit-graph
//! assisted when the snapshot has one) within the `git.reachability`
//! budget and its per-repository and per-principal concurrency limits. Tree
//! and blob wants pass only as a direct ref or tag target, as there is no
//! tree traversal; tag wants too, as only refs and other tags point at a
//! tag, and both are already in the fast set. Everything else fails closed
//! with one text, whether the object is unreachable, absent or past the
//! budget, so the answer doesn't tell them apart. For the same reason a
//! request with any want that isn't a ref tip takes its concurrency slots
//! before anything is looked up, so a busy repository answers every such
//! request alike.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use bytes::Bytes;
use gix::ObjectId;
use gix::objs::Kind;
use openxet_index::{Oid, RepoId};

use crate::pkt::err_pkt;
use crate::{Budget, SnapshotRepo};

/// Why a request was refused before git ran.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Refused {
    /// A want no ref of the snapshot reaches within the budget. The text
    /// begins as git's own refusal does (`upload-pack.c`, "not our ref").
    #[error(
        "upload-pack: not our ref {0}; fetch a branch or tag that contains it (objects no branch \
         or tag reaches, and commits deeper than the server's reachability budget, are refused)"
    )]
    NotOurRef(Oid),
    /// The repository has `git.reachability.max_concurrent_per_repo`
    /// walks running.
    #[error(
        "upload-pack: too many fetches by object id are being checked in this repository right \
         now; retry in a few seconds"
    )]
    RepoBusy,
    /// The principal has `git.reachability.max_concurrent_per_principal`
    /// walks running.
    #[error(
        "upload-pack: too many of your fetches by object id are being checked right now; retry \
         in a few seconds"
    )]
    PrincipalBusy,
}

impl Refused {
    /// Whether retrying later can succeed: over a concurrency limit, which
    /// HTTP answers with 429.
    #[must_use]
    pub fn retryable(&self) -> bool {
        matches!(self, Self::RepoBusy | Self::PrincipalBusy)
    }

    /// The refusal as the `ERR` packet the client reports.
    #[must_use]
    pub fn pkt(&self) -> Bytes {
        err_pkt(&self.to_string())
    }
}

/// The R22 check of one engine: its budget and the walks running per
/// repository and per principal. A clone shares the counts, so every walk
/// of the engine (R22's and the writer rule's thin-base walk, ADR 0004
/// item 3) is limited together.
#[derive(Debug, Clone)]
pub struct Reachability {
    budget: Budget,
    per_repo: Arc<Limiter<RepoId>>,
    per_principal: Arc<Limiter<String>>,
}

impl Reachability {
    /// A check with `budget` (`git.reachability`, ruling M2-R16).
    #[must_use]
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            per_repo: Arc::new(Limiter::new(budget.max_concurrent_per_repo)),
            per_principal: Arc::new(Limiter::new(budget.max_concurrent_per_principal)),
        }
    }

    /// Checks that every one of `wants` is reachable from `snapshot`'s
    /// refs, for principal `who`. A request whose wants are all ref tips
    /// takes no concurrency slot; any other takes both before the first
    /// object lookup.
    ///
    /// # Errors
    ///
    /// [`Refused::NotOurRef`] naming the first want that failed (an object
    /// that can't be read fails closed, logged), and [`Refused::RepoBusy`]
    /// or [`Refused::PrincipalBusy`] over a concurrency limit.
    pub async fn check(
        &self,
        snapshot: &SnapshotRepo,
        wants: &[Oid],
        who: &str,
    ) -> Result<(), Refused> {
        let tips: HashSet<Oid> = snapshot.refs.iter().map(|(_, oid)| *oid).collect();
        if wants.iter().all(|w| tips.contains(w)) {
            return Ok(());
        }
        let started = Instant::now();
        let _repo_slot = self
            .per_repo
            .try_acquire(snapshot.repo)
            .ok_or(Refused::RepoBusy)?;
        let _principal_slot = self
            .per_principal
            .try_acquire(who.to_owned())
            .ok_or(Refused::PrincipalBusy)?;
        let dir = snapshot.dir.path().to_path_buf();
        let refs: Vec<Oid> = snapshot.refs.iter().map(|(_, oid)| *oid).collect();
        let wants = wants.to_vec();
        let budget = self.budget;
        // gix's repository handle stays on the one blocking thread.
        blocking(move || {
            let Some(plan) = classify(&dir, &refs, wants, budget, started)? else {
                return Ok(());
            };
            plan.walk(budget, started)
        })
        .await
    }
}

impl Reachability {
    /// Takes one per-repository and one per-principal slot for a walk that
    /// isn't R22's own: the writer rule's thin-base walk shares R22's
    /// limits (ADR 0004 item 3), so concurrent pushes can't stack full
    /// walks. The slots are given back when the returned value is dropped.
    pub(crate) fn walk_slots(&self, repo: RepoId, who: &str) -> Result<WalkSlots, Refused> {
        let repo_slot = self.per_repo.try_acquire(repo).ok_or(Refused::RepoBusy)?;
        let principal_slot = self
            .per_principal
            .try_acquire(who.to_owned())
            .ok_or(Refused::PrincipalBusy)?;
        Ok(WalkSlots {
            _repo: repo_slot,
            _principal: principal_slot,
        })
    }
}

/// A walk's places in both limits, held while it runs.
pub(crate) struct WalkSlots {
    _repo: Slot<RepoId>,
    _principal: Slot<String>,
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, Refused> + Send + 'static,
) -> Result<T, Refused> {
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// The commit wants left for the walk, in request order, and where it
/// starts.
struct Walk {
    repo: gix::Repository,
    tips: Vec<ObjectId>,
    pending: Vec<Oid>,
}

/// Settles every want the fast set decides; `Ok(None)` when none is left
/// for the walk.
fn classify(
    dir: &Path,
    refs: &[Oid],
    wants: Vec<Oid>,
    budget: Budget,
    started: Instant,
) -> Result<Option<Walk>, Refused> {
    let first_off_tip = wants
        .iter()
        .find(|w| !refs.contains(w))
        .copied()
        .unwrap_or(Oid([0; 20]));
    let repo = open(dir).map_err(|reason| {
        tracing::warn!(%reason, "opening a snapshot for the reachability check failed");
        Refused::NotOurRef(first_off_tip)
    })?;
    let mut fast: HashSet<Oid> = HashSet::with_capacity(refs.len());
    let mut tips = Vec::with_capacity(refs.len());
    for tip in refs {
        let (chain, kind) = peel(&repo, *tip);
        if kind == Some(Kind::Commit) {
            tips.extend(chain.last().map(|c| to_gix(*c)));
        }
        fast.extend(chain);
        // Peeling many refs counts against the time budget too.
        if started.elapsed() >= budget.max_time {
            return Err(Refused::NotOurRef(first_off_tip));
        }
    }
    let mut pending = Vec::new();
    for want in wants {
        if fast.contains(&want) {
            continue;
        }
        match kind(&repo, want) {
            Ok(Some(Kind::Commit)) => pending.push(want),
            Ok(_) => return Err(Refused::NotOurRef(want)),
            Err(reason) => {
                tracing::warn!(%reason, "reading a want's type failed; refusing it");
                return Err(Refused::NotOurRef(want));
            }
        }
    }
    Ok((!pending.is_empty()).then_some(Walk {
        repo,
        tips,
        pending,
    }))
}

impl Walk {
    /// One breadth-first walk from the tips until every pending want is
    /// found or the budget is spent.
    fn walk(self, budget: Budget, started: Instant) -> Result<(), Refused> {
        let mut pending: HashSet<ObjectId> = self.pending.iter().map(|w| to_gix(*w)).collect();
        let refuse = |pending: &HashSet<ObjectId>| {
            let first = self
                .pending
                .iter()
                .find(|w| pending.contains(&to_gix(**w)))
                .copied();
            Refused::NotOurRef(first.unwrap_or(Oid([0; 20])))
        };
        if budget.max_objects == 0 || started.elapsed() >= budget.max_time {
            return Err(refuse(&pending));
        }
        let commits = self
            .repo
            .rev_walk(self.tips.iter().copied())
            .use_commit_graph(true)
            .all();
        let commits = match commits {
            Ok(commits) => commits,
            Err(e) => {
                tracing::warn!(error = %e, "starting the reachability walk failed");
                return Err(refuse(&pending));
            }
        };
        let mut visited = 0u64;
        for info in commits {
            let id = match info {
                Ok(info) => info.id,
                Err(e) => {
                    tracing::warn!(error = %e, "the reachability walk failed; refusing");
                    break;
                }
            };
            visited += 1;
            if pending.remove(&id) && pending.is_empty() {
                return Ok(());
            }
            if visited >= budget.max_objects
                || (visited.is_multiple_of(CLOCK_EVERY) && started.elapsed() >= budget.max_time)
            {
                break;
            }
        }
        Err(refuse(&pending))
    }
}

/// How many commits the walk visits between two looks at the clock.
const CLOCK_EVERY: u64 = 256;

/// Opens a snapshot for object lookups only, with no configuration from
/// outside the repository.
pub(crate) fn open(dir: &Path) -> Result<gix::Repository, String> {
    gix::open_opts(dir, gix::open::Options::isolated()).map_err(|e| e.to_string())
}

pub(crate) fn to_gix(oid: Oid) -> ObjectId {
    ObjectId::Sha1(oid.0)
}

fn from_gix(id: ObjectId) -> Option<Oid> {
    id.as_bytes().try_into().ok().map(Oid)
}

/// An object's type; `Ok(None)` if the snapshot doesn't hold it.
pub(crate) fn kind(repo: &gix::Repository, oid: Oid) -> Result<Option<Kind>, String> {
    repo.try_find_header(to_gix(oid))
        .map(|h| h.map(|h| h.kind()))
        .map_err(|e| e.to_string())
}

/// `oid` and, while it is a tag, each object its tag chain points at, with
/// the type of the last one (`None` if it is missing or unreadable). A
/// SHA-1 tag chain can't loop, so it ends.
pub(crate) fn peel(repo: &gix::Repository, oid: Oid) -> (Vec<Oid>, Option<Kind>) {
    let mut chain = vec![oid];
    let mut current = oid;
    loop {
        match kind(repo, current) {
            Ok(Some(Kind::Tag)) => {}
            Ok(k) => return (chain, k),
            Err(_) => return (chain, None),
        }
        let target = repo
            .find_tag(to_gix(current))
            .ok()
            .and_then(|t| t.target_id().ok())
            .and_then(|id| from_gix(id.detach()));
        match target {
            Some(next) => {
                chain.push(next);
                current = next;
            }
            None => return (chain, None),
        }
    }
}

/// A counting semaphore per key that never waits: a caller over the limit
/// is refused (ADR 0004 item 2: a retryable `ERR`, HTTP 429). Keys with no
/// holder are forgotten.
#[derive(Debug)]
struct Limiter<K> {
    max: Option<usize>,
    held: Mutex<HashMap<K, usize>>,
}

impl<K: Hash + Eq + Clone> Limiter<K> {
    fn new(max: Option<usize>) -> Self {
        Self {
            max,
            held: Mutex::new(HashMap::new()),
        }
    }

    fn try_acquire(self: &Arc<Self>, key: K) -> Option<Slot<K>> {
        let Some(max) = self.max else {
            return Some(Slot { limiter: None, key });
        };
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let count = held.entry(key.clone()).or_insert(0);
        if *count >= max {
            if *count == 0 {
                held.remove(&key);
            }
            return None;
        }
        *count += 1;
        Some(Slot {
            limiter: Some(Arc::clone(self)),
            key,
        })
    }
}

/// One held place of a [`Limiter`], given back on drop.
struct Slot<K: Hash + Eq> {
    limiter: Option<Arc<Limiter<K>>>,
    key: K,
}

impl<K: Hash + Eq> Drop for Slot<K> {
    fn drop(&mut self) {
        if let Some(limiter) = &self.limiter {
            let mut held = limiter.held.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(count) = held.get_mut(&self.key) {
                *count -= 1;
                if *count == 0 {
                    held.remove(&self.key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{Limiter, Refused};

    #[test]
    fn limiter_counts_per_key() {
        let limiter = Arc::new(Limiter::new(Some(2)));
        let a1 = limiter.try_acquire("a");
        let a2 = limiter.try_acquire("a");
        assert!(a1.is_some() && a2.is_some());
        assert!(limiter.try_acquire("a").is_none(), "a third slot for a");
        let b = limiter.try_acquire("b");
        assert!(b.is_some(), "keys are independent");
        drop(a1);
        assert!(limiter.try_acquire("a").is_some(), "a slot given back");
        drop((a2, b));
        assert!(limiter.held.lock().is_ok_and(|h| h.is_empty()));

        let none = Arc::new(Limiter::new(Some(0)));
        assert!(none.try_acquire("a").is_none());
        assert!(none.held.lock().is_ok_and(|h| h.is_empty()));
        let unlimited = Arc::new(Limiter::<&str>::new(None));
        let slots: Vec<_> = (0..100)
            .filter_map(|_| unlimited.try_acquire("a"))
            .collect();
        assert_eq!(slots.len(), 100);
    }

    #[test]
    fn refusals() {
        let oid = openxet_index::Oid([0xab; 20]);
        let text = Refused::NotOurRef(oid).to_string();
        assert!(
            text.starts_with(&format!("upload-pack: not our ref {oid}; ")),
            "{text}"
        );
        assert!(!Refused::NotOurRef(oid).retryable());
        assert!(Refused::RepoBusy.retryable() && Refused::PrincipalBusy.retryable());
        assert!(
            Refused::RepoBusy
                .pkt()
                .ends_with(b"retry in a few seconds\n")
        );
    }
}
