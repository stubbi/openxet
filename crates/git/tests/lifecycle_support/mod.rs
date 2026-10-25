//! The pack lifecycle harness (ADR 0004 items 4 and 5): a seeded
//! repository on a node whose pushes go through receive-pack, a client
//! repository holding every object a test makes, a Hub ref writer built on
//! the shared publish path, and the checks every lifecycle test makes:
//! invariant I1 on a fresh snapshot and R22 on what is served.

#![allow(dead_code, clippy::missing_panics_doc)]

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use openxet_git::{
    Engine, Lease, Publication, PublishError, Publisher, SentObjects, WalkLimits, WriterRefused,
    check_writer_rule, git_command,
};
use openxet_index::{CommitError, GitStore as _, Oid, PackRecord, PackState, RefUpdate, RepoId};
use openxet_storage::{Bucket as _, KeyKind, RepoIdHex};
use tempfile::TempDir;

use crate::common::fetch::{not_our_ref, upload, v2_fetch};
use crate::common::{World, fixture_repo, ok};
use crate::push_support::{RawResult, config, engine, must, pack_revs, raw_push, seeded};

/// One day in seconds.
pub const DAY: u64 = 24 * 3600;

/// A seeded repository and the node serving it.
pub struct Env {
    pub world: World,
    pub repo: RepoId,
    pub node: Arc<Engine>,
    pub fx: TempDir,
    pub client: TempDir,
}

/// A repository seeded from a `commits`-commit fixture (`main` and the tag
/// `v1.0` in one pack created a year ago), its node, and a client clone.
pub async fn env(commits: u32) -> Env {
    let world = World::new().await;
    let (repo, fx) = seeded(&world, commits).await;
    let node = engine(&world, config(&world, "node-a")).await;
    let client = ok(TempDir::new());
    let src = fixture_repo(&fx).to_string_lossy().into_owned();
    must(client.path(), &["clone", "-q", "--", &src, "work"]);
    Env {
        world,
        repo,
        node,
        fx,
        client,
    }
}

impl Env {
    pub fn work(&self) -> PathBuf {
        self.client.path().join("work")
    }

    /// Another node over the same index and bucket, with its own cache.
    pub async fn engine(&self, name: &str) -> Arc<Engine> {
        engine(&self.world, config(&self.world, name)).await
    }

    pub async fn refs(&self) -> Vec<(String, Oid)> {
        ok(self.world.index.snapshot(self.repo).await).refs
    }

    pub async fn db_ref(&self, name: &str) -> Option<Oid> {
        self.refs()
            .await
            .into_iter()
            .find(|(n, _)| n == name)
            .map(|(_, o)| o)
    }

    pub fn oid(&self, spec: &str) -> Oid {
        ok(must(&self.work(), &["rev-parse", spec]).trim().parse())
    }

    /// A new commit on `base` adding `path` with `contents`, made in the
    /// client without moving any of its refs.
    pub fn new_commit(&self, base: Oid, path: &str, contents: &[u8]) -> Oid {
        let w = self.work();
        must(&w, &["checkout", "-q", "--detach", &base.to_string()]);
        let file = w.join(path);
        if let Some(dir) = file.parent() {
            ok(std::fs::create_dir_all(dir));
        }
        ok(std::fs::write(&file, contents));
        must(&w, &["add", "--", path]);
        must(&w, &["commit", "-q", "-m", path]);
        self.oid("HEAD")
    }

    /// The objects reachable from `new` but from no ref, as a pack: empty
    /// when there are none, as git sends for a ref-only update.
    pub async fn pack_for(&self, new: Oid) -> Vec<u8> {
        let mut revs = new.to_string();
        for (_, tip) in self.refs().await {
            let _ = write!(revs, "\n^{tip}");
        }
        pack_revs(&self.work(), &revs, false)
    }

    /// Pushes `name` to `new` through receive-pack on `engine`, sending the
    /// objects no ref reaches.
    pub async fn push_on(&self, engine: &Engine, name: &str, new: Oid) -> RawResult {
        let old = self.db_ref(name).await;
        let pack = self.pack_for(new).await;
        raw_push(
            engine,
            self.repo,
            "pusher",
            &[(old, Some(new), name)],
            Some(pack),
        )
        .await
    }

    pub async fn push(&self, name: &str, new: Oid) -> RawResult {
        self.push_on(&self.node, name, new).await
    }

    /// Deletes `name` through receive-pack.
    pub async fn delete(&self, name: &str) -> RawResult {
        let old = self.db_ref(name).await;
        raw_push(&self.node, self.repo, "pusher", &[(old, None, name)], None).await
    }

    /// Takes the repack lease for `holder`, which must be free.
    pub async fn lease(&self, holder: &str) -> Lease {
        ok(self
            .world
            .index
            .acquire_lease(self.repo, holder, std::time::Duration::from_mins(10))
            .await)
        .unwrap_or_else(|| panic!("the lease is free for {holder}"))
    }

    /// Expires whatever lease the repository has, then takes it for
    /// `holder`: a takeover.
    pub async fn take_over(&self, holder: &str) -> Lease {
        ok(
            sqlx::query("UPDATE repo_leases SET expires_at = 0 WHERE repo = ?")
                .bind(&self.repo.0[..])
                .execute(&self.world.sql().await)
                .await,
        );
        self.lease(holder).await
    }

    /// Moves every time the index holds for the repository `secs` into the
    /// past: ref log rows and pack times, so its history looks that old.
    pub async fn age(&self, secs: u64) {
        let pool = self.world.sql().await;
        let secs = i64::try_from(secs).unwrap_or(i64::MAX);
        for q in [
            "UPDATE ref_log SET at = at - ? WHERE repo = ?",
            "UPDATE packs SET created_at = created_at - ?1, superseded_at = superseded_at - ?1, \
             committed_at = committed_at - ?1 WHERE repo = ?2",
        ] {
            ok(sqlx::query(q)
                .bind(secs)
                .bind(&self.repo.0[..])
                .execute(&pool)
                .await);
        }
    }

    pub async fn rows(&self) -> Vec<PackRecord> {
        ok(self.world.index.packs(self.repo).await)
    }

    pub async fn state(&self, name: &str) -> Option<PackState> {
        self.rows()
            .await
            .into_iter()
            .find(|r| r.pack.name == name)
            .map(|r| r.pack.state)
    }

    pub async fn committed_names(&self) -> BTreeSet<String> {
        ok(self.world.index.snapshot(self.repo).await)
            .packs
            .into_iter()
            .map(|p| p.name)
            .collect()
    }

    /// The repository's pack and multi-pack-index files in the bucket, as
    /// key strings.
    pub async fn bucket_files(&self) -> BTreeSet<String> {
        let hex = ok(RepoIdHex::parse(&self.repo.to_string()));
        let mut out = BTreeSet::new();
        for kind in [KeyKind::Pack, KeyKind::Midx] {
            for key in ok(self.world.bucket.list_under(hex, kind).await) {
                out.insert(key.to_string());
            }
        }
        out
    }
}

/// `git` in `dir` with `input` on stdin; panics on failure.
pub fn must_with_input(dir: &std::path::Path, args: &[&str], input: &str) -> String {
    crate::common::git_with_input(dir, args, input)
}

/// Invariant I1 (ADR 0004 item 3): every ref's closure lies in the
/// committed packs, checked with `git rev-list --objects` on a fresh
/// snapshot of `engine`.
pub async fn assert_i1(engine: &Engine, repo: RepoId) {
    let snap = ok(engine.snapshot(repo).await);
    for (name, oid) in &snap.refs {
        let out = ok(git_command(engine.git(), &snap, None)
            .args(["rev-list", "--objects", "--quiet", &oid.to_string()])
            .output()
            .await);
        assert!(
            out.status.success(),
            "I1: {name} at {oid} has objects outside the committed packs: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Whether a fresh snapshot of `engine` holds `oid`.
pub async fn has_object(engine: &Engine, repo: RepoId, oid: Oid) -> bool {
    let snap = ok(engine.snapshot(repo).await);
    ok(git_command(engine.git(), &snap, None)
        .args(["cat-file", "-e", &oid.to_string()])
        .output()
        .await)
    .status
    .success()
}

/// Whether R22 refuses a v2 `want` of `oid` (ADR 0004 item 2): the answer
/// is openxet's `ERR`, and no pack is sent.
pub async fn r22_refuses(engine: &Engine, repo: RepoId, oid: Oid) -> bool {
    let answer = upload(engine, repo, 2, v2_fetch(&[oid])).await;
    let err = not_our_ref(oid);
    let refused = answer.windows(err.len()).any(|w| w == err.as_slice());
    if refused {
        assert!(
            !answer.windows(4).any(|w| w == b"PACK"),
            "a refused want sent a pack"
        );
    }
    refused
}

/// A Hub ref write through the shared publish path (ADR 0004 item 6): a
/// snapshot, the writer reachability rule with no pack, then the fenced
/// commit transaction. Task 10's `Engine::hub_ref_write` replaces this
/// driver; until then the lifecycle tests use it.
pub struct HubWrite<'a> {
    pub engine: &'a Engine,
    pub repo: RepoId,
    pub update: RefUpdate,
}

/// What [`HubWrite::check`] read: the generation to commit against.
pub struct Checked {
    pub full_repack_gen: u64,
}

impl HubWrite<'_> {
    /// Steps 1 and 2: the snapshot and the writer rule. A refusal ends the
    /// write before any transaction.
    pub async fn check(&self) -> Result<Checked, WriterRefused> {
        let snap = ok(self.engine.snapshot(self.repo).await);
        if let Some(new) = self.update.new {
            check_writer_rule(
                &snap,
                &[new],
                &SentObjects::default(),
                &openxet_git::Budget::THIN_BASES,
                WalkLimits {
                    reach: self.engine.reachability(),
                    who: "hub",
                },
            )
            .await?;
        }
        Ok(Checked {
            full_repack_gen: snap.full_repack_gen,
        })
    }

    /// Step 3: the commit transaction, fenced by the checked generation.
    pub async fn commit(&self, world: &World, checked: &Checked) -> Result<(), PublishError> {
        Publisher::new(world.index.clone(), world.bucket.clone())
            .publish(Publication {
                repo: self.repo,
                pack: None,
                updates: std::slice::from_ref(&self.update),
                full_repack_gen: checked.full_repack_gen,
                actor: "hub",
                request: "hub",
            })
            .await
            .map(drop)
    }

    /// The whole write, retried from a fresh snapshot up to 3 times on the
    /// fence or a moved ref, as Hub writes are (ADR 0004 item 3).
    pub async fn run(&self, world: &World) -> Result<(), String> {
        for _ in 0..3 {
            let checked = self.check().await.map_err(|e| e.to_string())?;
            match self.commit(world, &checked).await {
                Ok(()) => return Ok(()),
                Err(PublishError::Commit(CommitError::Fenced | CommitError::RefMismatch(_))) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
        Err("the Hub write kept failing on the fence".to_owned())
    }
}
