//! `lifecycle_props` (ADR 0004, "Not yet proven: the pack lifecycle must be
//! prototyped first"): random interleavings of pushes, Hub ref creates,
//! updates and deletes, Hub commits, geometric and full repacks, lease
//! takeovers, sweeps, aging, hydration on a second node, the bucket ref log
//! and `reindex` with restore and discard. After every step invariant I1
//! holds on a fresh snapshot (every ref's closure is in committed packs,
//! checked with `git rev-list --objects`), and nothing R22 refuses is
//! served: every commit the test made that no ref reaches is refused, and
//! reachable ones are served.
//!
//! Hub ref writes and Hub commits go through the shared publish path with
//! this harness's own driver ([`HubWrite`], and a pack built from the
//! client) until Task 10's Hub object layer lands.
//!
//! `PROPTEST_CASES` sets the number of cases (default 12).

mod common;
mod lifecycle_support;
mod push_support;

use std::collections::HashSet;
use std::fmt::Write as _;
use std::sync::Arc;

use openxet_git::{
    DEFAULT_GRACE, Engine, GitError, Lease, PackFiles, Publication, PublishError, Publisher,
    derive_bucket_ref_log, git_command, reindex_discard, reindex_repo, reindex_restore,
    repack_full, repack_geometric, sweep_packs,
};
use openxet_index::{CommitError, GitStore as _, Oid, RefUpdate, UnplacedKind};
use openxet_storage::PackName;
use proptest::prelude::*;

use common::{ok, pack_objects};
use lifecycle_support::{DAY, Env, HubWrite, assert_i1, env, r22_refuses};

const BRANCHES: [&str; 3] = ["refs/heads/main", "refs/heads/b1", "refs/heads/b2"];
const HUB_REFS: [&str; 4] = [
    "refs/heads/h1",
    "refs/heads/h2",
    "refs/tags/t1",
    "refs/tags/t2",
];

#[derive(Debug, Clone)]
enum Op {
    /// A new commit on a branch, pushed through receive-pack.
    Push(usize),
    /// A branch force-pushed back to its parent.
    ForceBack(usize),
    /// A branch other than `main` deleted by a push.
    Delete(usize),
    /// A Hub ref created, moved to a commit the test made (which the
    /// writer rule may refuse), or deleted.
    HubRef(usize, usize),
    /// A Hub commit on a branch.
    HubCommit(usize),
    Geometric,
    Full,
    /// Another node takes the lease over; the old holder's repacks fail.
    TakeOver,
    /// A repack under a lease that was taken over.
    StaleRepack,
    /// A sweep now, or three grace periods ahead.
    Sweep(bool),
    /// Everything in the index looks a grace period and a day older.
    Age,
    /// The checks run on a second node with its own cache.
    Hydrate,
    DeriveRefLog,
    /// `reindex`, after deriving the bucket ref log or not, then a restore
    /// of an unplaced entry, a discard, or nothing.
    Reindex(bool, u8),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..3usize).prop_map(Op::Push),
        2 => (0..3usize).prop_map(Op::ForceBack),
        1 => (1..3usize).prop_map(Op::Delete),
        2 => (0..4usize, 0..64usize).prop_map(|(r, t)| Op::HubRef(r, t)),
        2 => (0..3usize).prop_map(Op::HubCommit),
        2 => Just(Op::Geometric),
        2 => Just(Op::Full),
        1 => Just(Op::TakeOver),
        1 => Just(Op::StaleRepack),
        1 => any::<bool>().prop_map(Op::Sweep),
        1 => Just(Op::Age),
        1 => Just(Op::Hydrate),
        1 => Just(Op::DeriveRefLog),
        1 => (any::<bool>(), 0..3u8).prop_map(|(d, t)| Op::Reindex(d, t)),
    ]
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12)
}

/// The model: the commits made, the lease, the restores.
struct Model {
    env: Env,
    other: Arc<Engine>,
    commits: Vec<Oid>,
    holder: usize,
    lease: Lease,
    stale: Option<Lease>,
    restores: usize,
}

impl Model {
    /// The ref's target, else `main`'s, else the fixture's tip: a reindex
    /// from a bucket ref log that lags may leave `main` out.
    async fn tip_or_main(&self, name: &str) -> Oid {
        match self.env.db_ref(name).await {
            Some(oid) => oid,
            None => match self.env.db_ref(BRANCHES[0]).await {
                Some(oid) => oid,
                None => self.env.oid("refs/remotes/origin/main"),
            },
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn step(&mut self, op: &Op) {
        let env = &self.env;
        match op {
            Op::Push(b) => {
                let base = self.tip_or_main(BRANCHES[*b]).await;
                let n = self.commits.len();
                let new =
                    env.new_commit(base, &format!("push/{n}.txt"), format!("{n}\n").as_bytes());
                self.commits.push(new);
                let r = env.push(BRANCHES[*b], new).await;
                assert!(r.ok(BRANCHES[*b]), "{op:?}: {r:?}");
            }
            Op::ForceBack(b) => {
                let Some(tip) = env.db_ref(BRANCHES[*b]).await else {
                    return;
                };
                let parents = common::git(
                    &env.work(),
                    &["rev-list", "--parents", "-n1", &tip.to_string()],
                );
                let Some(parent) = parents.split_whitespace().nth(1) else {
                    return;
                };
                let parent: Oid = ok(parent.parse());
                let r = env.push(BRANCHES[*b], parent).await;
                assert!(r.ok(BRANCHES[*b]), "{op:?}: {r:?}");
            }
            Op::Delete(b) => {
                if env.db_ref(BRANCHES[*b]).await.is_some() {
                    let r = env.delete(BRANCHES[*b]).await;
                    assert!(r.ok(BRANCHES[*b]), "{op:?}: {r:?}");
                }
            }
            Op::HubRef(r, t) => {
                let name = HUB_REFS[*r];
                let old = env.db_ref(name).await;
                let new = if t % 5 == 0 && old.is_some() {
                    None
                } else if self.commits.is_empty() {
                    Some(self.tip_or_main(BRANCHES[0]).await)
                } else {
                    Some(self.commits[t % self.commits.len()])
                };
                if old.is_none() && new.is_none() {
                    return;
                }
                let write = HubWrite {
                    engine: &env.node,
                    repo: env.repo,
                    update: RefUpdate {
                        name: name.to_owned(),
                        old,
                        new,
                    },
                };
                let reachable = match new {
                    Some(oid) => self.reachable().await.contains(&oid),
                    None => true,
                };
                let result = write.run(&env.world).await;
                assert_eq!(
                    result.is_ok(),
                    reachable,
                    "{op:?}: a Hub ref write succeeds exactly for a reachable target: {result:?}"
                );
            }
            Op::HubCommit(b) => {
                let name = BRANCHES[*b];
                let n = self.commits.len();
                for attempt in 0..3 {
                    let snap = ok(env.world.index.snapshot(env.repo).await);
                    let old = snap.refs.iter().find(|(r, _)| r == name).map(|(_, o)| *o);
                    let base = match old {
                        Some(o) => o,
                        None => self.tip_or_main(BRANCHES[0]).await,
                    };
                    let new = env.new_commit(base, &format!("hub/{n}-{attempt}.txt"), b"hub\n");
                    let mut revs = new.to_string();
                    for (_, tip) in &snap.refs {
                        let _ = write!(revs, "\n^{tip}");
                    }
                    let out = env.world.dir.path().join(format!("hub-{n}-{attempt}"));
                    let files = pack_objects(&env.work(), &revs, &out);
                    let published =
                        Publisher::new(env.world.index.clone(), env.world.bucket.clone())
                            .publish(Publication {
                                repo: env.repo,
                                pack: Some(&PackFiles {
                                    dir: files.dir.clone(),
                                    name: ok(PackName::parse(&files.name)),
                                }),
                                updates: &[RefUpdate {
                                    name: name.to_owned(),
                                    old,
                                    new: Some(new),
                                }],
                                full_repack_gen: snap.full_repack_gen,
                                actor: "hub",
                                request: "hub-commit",
                            })
                            .await;
                    self.commits.push(new);
                    match published {
                        Ok(_) => return,
                        Err(PublishError::Commit(
                            CommitError::Fenced | CommitError::RefMismatch(_),
                        )) => {}
                        Err(e) => panic!("{op:?}: {e}"),
                    }
                }
                panic!("{op:?}: a sequential Hub commit kept failing");
            }
            Op::Geometric => {
                ok(repack_geometric(&env.node, env.repo, &self.lease).await);
            }
            Op::Full => {
                ok(repack_full(&env.node, env.repo, &self.lease, DEFAULT_GRACE).await);
            }
            Op::TakeOver => {
                self.holder += 1;
                let next = env.take_over(&format!("node-{}", self.holder)).await;
                self.stale = Some(std::mem::replace(&mut self.lease, next));
            }
            Op::StaleRepack => {
                let Some(stale) = &self.stale else { return };
                let before = env.committed_names().await;
                match repack_geometric(&env.node, env.repo, stale).await {
                    Err(GitError::RepackFenced) => {}
                    Ok(report) => assert!(report.published.is_empty(), "{op:?}: {report:?}"),
                    Err(e) => panic!("{op:?}: {e}"),
                }
                assert_eq!(env.committed_names().await, before, "{op:?}");
            }
            Op::Sweep(far) => {
                let now = unix_now() + if *far { 3 * 30 * DAY } else { 0 };
                ok(sweep_packs(&env.node, env.repo, DEFAULT_GRACE, now, false).await);
            }
            Op::Age => env.age(31 * DAY).await,
            Op::Hydrate => {
                assert_i1(&self.other, env.repo).await;
                self.check_r22(&self.other).await;
            }
            Op::DeriveRefLog => {
                ok(derive_bucket_ref_log(&env.node, env.repo).await);
            }
            Op::Reindex(derive, then) => {
                if *derive {
                    ok(derive_bucket_ref_log(&env.node, env.repo).await);
                }
                ok(reindex_repo(&env.node, env.repo).await);
                match then {
                    1 => {
                        let entry = ok(env.world.index.unplaced(env.repo).await)
                            .into_iter()
                            .find_map(|u| match u.kind {
                                UnplacedKind::Commit(o) => Some((o, "heads")),
                                UnplacedKind::Tag(o) => Some((o, "tags")),
                                UnplacedKind::Edge(_) | UnplacedKind::Pack => None,
                            });
                        if let Some((oid, kind)) = entry {
                            self.restores += 1;
                            let name = format!("refs/{kind}/restored-{}", self.restores);
                            match reindex_restore(&env.node, env.repo, oid, &name).await {
                                Ok(()) => assert_eq!(env.db_ref(&name).await, Some(oid)),
                                Err(GitError::Restore { reason, .. })
                                    if reason.contains("incomplete") => {}
                                Err(e) => panic!("{op:?}: {e}"),
                            }
                        }
                    }
                    2 => {
                        ok(reindex_discard(&env.node, env.repo, false).await);
                        assert!(ok(env.world.index.unplaced(env.repo).await).is_empty());
                    }
                    _ => {}
                }
            }
        }
    }

    /// Every object the refs reach, in a fresh snapshot.
    async fn reachable(&self) -> HashSet<Oid> {
        let snap = ok(self.env.node.snapshot(self.env.repo).await);
        let out = ok(git_command(self.env.node.git(), &snap, None)
            .args(["rev-list", "--all"])
            .output()
            .await);
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.parse().ok())
            .collect()
    }

    /// R22: every commit the test made that no ref reaches is refused, and
    /// some reachable ones are served.
    async fn check_r22(&self, engine: &Engine) {
        let reachable = self.reachable().await;
        let mut unreachable = self.commits.iter().filter(|c| !reachable.contains(c));
        for c in unreachable.by_ref().take(3) {
            assert!(
                r22_refuses(engine, self.env.repo, *c).await,
                "R22 serves {c}, which no ref reaches"
            );
        }
        for c in self
            .commits
            .iter()
            .filter(|c| reachable.contains(c))
            .take(2)
        {
            assert!(
                !r22_refuses(engine, self.env.repo, *c).await,
                "R22 refuses {c}, which a ref reaches"
            );
        }
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

async fn run(ops: Vec<Op>) {
    let env = env(6).await;
    let other = env.engine("node-b").await;
    let lease = env.lease("node-0").await;
    let mut model = Model {
        env,
        other,
        commits: Vec::new(),
        holder: 0,
        lease,
        stale: None,
        restores: 0,
    };
    // The seed reaches the bucket ref log, as the server's task does
    // within seconds.
    ok(derive_bucket_ref_log(&model.env.node, model.env.repo).await);
    for op in &ops {
        model.step(op).await;
        assert_i1(&model.env.node, model.env.repo).await;
        model.check_r22(&model.env.node).await;
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn lifecycle_props(ops in prop::collection::vec(op(), 1..10)) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build();
        ok(runtime).block_on(run(ops));
    }
}
