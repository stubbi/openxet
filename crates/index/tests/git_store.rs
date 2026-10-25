//! The SQLite index against the `GitStore` contract: snapshots, the commit
//! transaction (ADR 0004 item 3), pack states (items 3 to 5), repack leases
//! and publishes (item 5) and unplaced entries (item 4, ruling R24).

use std::sync::Arc;
use std::time::Duration;

use openxet_index::{
    CommitError, CommitTx, GitStore, IndexError, Oid, Pack, PackId, PackState, RefUpdate,
    RepackPublish, Repo, RepoId, Repos, SqliteIndex, Unplaced, UnplacedKind,
};
use tempfile::TempDir;
use tokio::sync::Barrier;
use tokio::task::JoinSet;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("index call failed: {e}"),
    }
}

async fn open(dir: &TempDir) -> SqliteIndex {
    ok(SqliteIndex::open(&dir.path().join("index.sqlite")).await)
}

async fn new_repo(index: &SqliteIndex, name: &str) -> RepoId {
    let repo = Repo {
        id: RepoId::generate(),
        namespace: "org".to_owned(),
        name: name.to_owned(),
        private: false,
        labels: vec!["model".to_owned()],
        created_at: 1_761_400_000,
        full_repack_gen: 0,
    };
    ok(index.create_repo(&repo).await);
    repo.id
}

fn oid(n: u8) -> Oid {
    Oid([n; 20])
}

fn pending(repo: RepoId, name: &str) -> Pack {
    Pack {
        id: PackId::generate(),
        repo,
        name: name.to_owned(),
        state: PackState::Pending,
        created_at: 1_761_400_100,
        bytes: 4096,
        midx_gen: None,
    }
}

fn update(name: &str, old: Option<u8>, new: Option<u8>) -> RefUpdate {
    RefUpdate {
        name: name.to_owned(),
        old: old.map(oid),
        new: new.map(oid),
    }
}

fn tx(repo: RepoId, updates: &[RefUpdate], pack: Option<PackId>, generation: u64) -> CommitTx<'_> {
    CommitTx {
        repo,
        updates,
        pack,
        full_repack_gen: generation,
        actor: "alice",
        request: "req-1",
    }
}

/// Commits a fresh pack moving `refs/heads/main` from `old` to `new`.
async fn push(index: &SqliteIndex, repo: RepoId, old: Option<u8>, new: u8) -> Pack {
    let pack = pending(repo, &format!("pack-{new}"));
    ok(index.insert_pack(&pack).await);
    let updates = [update("refs/heads/main", old, Some(new))];
    ok(index.commit(tx(repo, &updates, Some(pack.id), 0)).await);
    Pack {
        state: PackState::Committed,
        ..pack
    }
}

#[tokio::test]
async fn snapshot_reads_refs_and_committed_packs_only() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let other = new_repo(&index, "b").await;

    let empty = ok(index.snapshot(repo).await);
    assert!(empty.refs.is_empty() && empty.packs.is_empty());
    assert_eq!(empty.full_repack_gen, 0);

    let committed = push(&index, repo, None, 1).await;
    let tag = [update("refs/tags/v1", None, Some(1))];
    ok(index.commit(tx(repo, &tag, None, 0)).await);
    // Every other state stays out of every snapshot (ADR 0004 item 4).
    let still_pending = pending(repo, "pending");
    ok(index.insert_pack(&still_pending).await);
    let lost = pending(repo, "lost");
    ok(index.insert_pack(&lost).await);
    assert!(ok(index
        .mark_pack(lost.id, PackState::Pending, PackState::Lost)
        .await));
    let expired = pending(repo, "expired");
    ok(index.insert_pack(&expired).await);
    assert!(ok(index
        .mark_pack(expired.id, PackState::Pending, PackState::Expired)
        .await));
    push(&index, other, None, 9).await;

    let snap = ok(index.snapshot(repo).await);
    assert_eq!(
        snap.refs,
        vec![
            ("refs/heads/main".to_owned(), oid(1)),
            ("refs/tags/v1".to_owned(), oid(1)),
        ]
    );
    assert_eq!(snap.packs, vec![committed]);

    match index.snapshot(RepoId::generate()).await {
        Err(IndexError::UnknownRepo { .. }) => {}
        other => panic!("expected UnknownRepo, got {other:?}"),
    }
}

#[tokio::test]
async fn commit_applies_all_updates_or_none() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    push(&index, repo, None, 1).await;
    let dev = [update("refs/heads/dev", None, Some(1))];
    ok(index.commit(tx(repo, &dev, None, 0)).await);
    let before = ok(index.snapshot(repo).await);

    let pack = pending(repo, "two");
    ok(index.insert_pack(&pack).await);
    let updates = [
        update("refs/heads/main", Some(1), Some(2)),
        update("refs/heads/dev", Some(7), Some(2)),
    ];
    match index.commit(tx(repo, &updates, Some(pack.id), 0)).await {
        Err(CommitError::RefMismatch(name)) => assert_eq!(name, "refs/heads/dev"),
        other => panic!("expected RefMismatch, got {other:?}"),
    }
    assert_eq!(ok(index.snapshot(repo).await), before);
    // The pack wasn't committed either, so the writer can mark it lost.
    assert!(ok(index
        .mark_pack(pack.id, PackState::Pending, PackState::Lost)
        .await));

    // A create of an existing ref and a delete with the wrong target fail
    // the same way.
    for bad in [
        update("refs/heads/main", None, Some(3)),
        update("refs/heads/main", Some(2), None),
    ] {
        match index
            .commit(tx(repo, std::slice::from_ref(&bad), None, 0))
            .await
        {
            Err(CommitError::RefMismatch(name)) => assert_eq!(name, "refs/heads/main"),
            other => panic!("expected RefMismatch for {bad:?}, got {other:?}"),
        }
    }
    assert_eq!(ok(index.snapshot(repo).await), before);

    // Update, create and delete together.
    let all = [
        update("refs/heads/main", Some(1), Some(2)),
        update("refs/heads/new", None, Some(2)),
        update("refs/heads/dev", Some(1), None),
    ];
    ok(index.commit(tx(repo, &all, None, 0)).await);
    assert_eq!(
        ok(index.snapshot(repo).await).refs,
        vec![
            ("refs/heads/main".to_owned(), oid(2)),
            ("refs/heads/new".to_owned(), oid(2)),
        ]
    );

    // Commands that can't be a transaction are refused before any write.
    for invalid in [
        vec![],
        vec![update("refs/heads/x", None, None)],
        vec![
            update("refs/heads/y", None, Some(1)),
            update("refs/heads/y", None, Some(2)),
        ],
    ] {
        match index.commit(tx(repo, &invalid, None, 0)).await {
            Err(CommitError::Database(IndexError::InvalidInput(_))) => {}
            other => panic!("expected InvalidInput for {invalid:?}, got {other:?}"),
        }
    }
    let missing = RepoId::generate();
    match index.commit(tx(missing, &dev, None, 0)).await {
        Err(CommitError::Database(IndexError::UnknownRepo { repo })) => assert_eq!(repo, missing),
        other => panic!("expected UnknownRepo, got {other:?}"),
    }
}

#[tokio::test]
async fn commit_requires_pending_pack() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let other = new_repo(&index, "b").await;
    // GC expired it: it can never be committed (ADR 0004 item 3).
    let expired = pending(repo, "expired");
    ok(index.insert_pack(&expired).await);
    assert!(ok(index
        .mark_pack(expired.id, PackState::Pending, PackState::Expired)
        .await));
    // Already committed by an earlier transaction: never reused.
    let used = push(&index, repo, None, 5).await;
    let main_from_5 = [update("refs/heads/main", Some(5), Some(1))];
    // Another repository's pending pack.
    let foreign = pending(other, "foreign");
    ok(index.insert_pack(&foreign).await);
    // A pack id that was never recorded.
    let unknown = PackId::generate();

    for pack in [expired.id, used.id, foreign.id, unknown] {
        match index.commit(tx(repo, &main_from_5, Some(pack), 0)).await {
            Err(CommitError::PackNotPending) => {}
            other => panic!("expected PackNotPending, got {other:?}"),
        }
    }
    assert_eq!(
        ok(index.snapshot(repo).await).refs,
        vec![("refs/heads/main".to_owned(), oid(5))]
    );

    // A pack in another state can't be inserted.
    let mut committed = pending(repo, "c");
    committed.state = PackState::Committed;
    match index.insert_pack(&committed).await {
        Err(IndexError::InvalidInput(_)) => {}
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    match index.insert_pack(&foreign).await {
        Err(IndexError::InvalidInput(_)) => {}
        other => panic!("expected InvalidInput for a reused id, got {other:?}"),
    }
    match index.insert_pack(&pending(RepoId::generate(), "x")).await {
        Err(IndexError::UnknownRepo { .. }) => {}
        other => panic!("expected UnknownRepo, got {other:?}"),
    }
}

#[tokio::test]
async fn commit_fenced_by_full_repack_gen() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let base = push(&index, repo, None, 1).await;

    // A writer takes its snapshot, then a full repack publishes.
    let snap = ok(index.snapshot(repo).await);
    let lease = ok(index
        .acquire_lease(repo, "node-a", Duration::from_mins(1))
        .await)
    .unwrap_or_else(|| panic!("lease"));
    let out = pending(repo, "full");
    ok(index.insert_pack(&out).await);
    let high_water = ok(index.ref_log_since(repo, 0).await)
        .last()
        .map_or(0, |r| r.seq);
    ok(index
        .publish_repack(RepackPublish {
            repo,
            holder: "node-a",
            epoch: lease.epoch,
            outputs: &[out.id],
            inputs: &[base.id],
            ref_log_high_water: Some(high_water),
        })
        .await);
    assert_eq!(
        ok(index.snapshot(repo).await).full_repack_gen,
        snap.full_repack_gen + 1
    );

    let pack = pending(repo, "late");
    ok(index.insert_pack(&pack).await);
    let updates = [update("refs/heads/main", Some(1), Some(2))];
    match index
        .commit(tx(repo, &updates, Some(pack.id), snap.full_repack_gen))
        .await
    {
        Err(CommitError::Fenced) => {}
        other => panic!("expected Fenced, got {other:?}"),
    }
    // Rolled back as a whole: the pack is still pending, the ref unmoved.
    assert!(ok(index
        .mark_pack(pack.id, PackState::Pending, PackState::Lost)
        .await));
    let now = ok(index.snapshot(repo).await);
    assert_eq!(now.refs, vec![("refs/heads/main".to_owned(), oid(1))]);

    // A retry from a fresh snapshot commits.
    let retry = pending(repo, "retry");
    ok(index.insert_pack(&retry).await);
    ok(index
        .commit(tx(repo, &updates, Some(retry.id), now.full_repack_gen))
        .await);
}

/// A ref log row's `(ref, old, new, actor, request)`.
type LogRow<'a> = (&'a str, Option<Oid>, Option<Oid>, &'a str, &'a str);

#[tokio::test]
async fn commit_writes_one_ref_log_row_per_update() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let other = new_repo(&index, "b").await;
    push(&index, repo, None, 1).await;
    push(&index, other, None, 1).await;
    let updates = [
        update("refs/heads/main", Some(1), Some(2)),
        update("refs/tags/v1", None, Some(2)),
    ];
    ok(index
        .commit(CommitTx {
            actor: "bob",
            request: "req-2",
            ..tx(repo, &updates, None, 0)
        })
        .await);
    // A failed transaction writes none.
    let stale = [update("refs/heads/main", Some(1), None)];
    assert!(index.commit(tx(repo, &stale, None, 0)).await.is_err());
    let delete = [update("refs/tags/v1", Some(2), None)];
    ok(index.commit(tx(repo, &delete, None, 0)).await);

    let log = ok(index.ref_log_since(repo, 0).await);
    let rows: Vec<LogRow<'_>> = log
        .iter()
        .map(|r| {
            assert_eq!(r.repo, repo);
            assert!(r.at > 1_700_000_000, "{r:?}");
            (
                r.name.as_str(),
                r.old,
                r.new,
                r.actor.as_str(),
                r.request.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("refs/heads/main", None, Some(oid(1)), "alice", "req-1"),
            (
                "refs/heads/main",
                Some(oid(1)),
                Some(oid(2)),
                "bob",
                "req-2"
            ),
            ("refs/tags/v1", None, Some(oid(2)), "bob", "req-2"),
            ("refs/tags/v1", Some(oid(2)), None, "alice", "req-1"),
        ]
    );
    assert!(log.windows(2).all(|w| w[0].seq < w[1].seq));
    let since = ok(index.ref_log_since(repo, log[1].seq).await);
    assert_eq!(since, log[2..]);
    assert_eq!(ok(index.ref_log_since(other, 0).await).len(), 1);
}

#[tokio::test]
async fn mark_pack_is_guarded() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let pack = pending(repo, "p");
    ok(index.insert_pack(&pack).await);

    assert!(ok(index
        .mark_pack(pack.id, PackState::Pending, PackState::Lost)
        .await));
    assert!(!ok(index
        .mark_pack(pack.id, PackState::Pending, PackState::Lost)
        .await));
    assert!(!ok(index
        .mark_pack(pack.id, PackState::Pending, PackState::Expired)
        .await));

    let gc = pending(repo, "gc");
    ok(index.insert_pack(&gc).await);
    assert!(ok(index
        .mark_pack(gc.id, PackState::Pending, PackState::Expired)
        .await));
    // Committing happens only in a commit or publish transaction, and an
    // expired pack never comes back (ADR 0004 item 3).
    for (from, to) in [
        (PackState::Expired, PackState::Committed),
        (PackState::Pending, PackState::Committed),
        (PackState::Lost, PackState::Pending),
        (PackState::Committed, PackState::Superseded),
        (PackState::Recovered, PackState::Committed),
    ] {
        match index.mark_pack(gc.id, from, to).await {
            Err(IndexError::InvalidInput(_)) => {}
            other => panic!("expected InvalidInput for {from:?} -> {to:?}, got {other:?}"),
        }
    }
    let main = [update("refs/heads/main", None, Some(1))];
    match index.commit(tx(repo, &main, Some(gc.id), 0)).await {
        Err(CommitError::PackNotPending) => {}
        other => panic!("expected PackNotPending, got {other:?}"),
    }
    assert!(!ok(index
        .mark_pack(PackId::generate(), PackState::Pending, PackState::Lost)
        .await));
}

#[tokio::test]
async fn lease_epoch_increases_and_expires() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let other = new_repo(&index, "b").await;
    let ttl = Duration::from_millis(400);

    let first =
        ok(index.acquire_lease(repo, "node-a", ttl).await).unwrap_or_else(|| panic!("first lease"));
    assert_eq!(first.holder, "node-a");
    assert_eq!(first.repo, repo);
    assert_eq!(ok(index.acquire_lease(repo, "node-b", ttl).await), None);
    // Leases are per repository.
    assert!(ok(index.acquire_lease(other, "node-b", ttl).await).is_some());

    // The holder may take it again, as a new epoch.
    let renewed = ok(index.acquire_lease(repo, "node-a", ttl).await)
        .unwrap_or_else(|| panic!("renewed lease"));
    assert!(renewed.epoch > first.epoch);
    assert!(renewed.expires_at_ms >= first.expires_at_ms);

    tokio::time::sleep(ttl + Duration::from_millis(100)).await;
    let taken = ok(index.acquire_lease(repo, "node-b", ttl).await)
        .unwrap_or_else(|| panic!("expired lease taken over"));
    assert_eq!(taken.holder, "node-b");
    assert!(taken.epoch > renewed.epoch);

    match index.acquire_lease(RepoId::generate(), "node-a", ttl).await {
        Err(IndexError::UnknownRepo { .. }) => {}
        other => panic!("expected UnknownRepo, got {other:?}"),
    }
}

#[tokio::test]
async fn publish_repack_rejects_stale_epoch_and_advanced_ref_log() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let a = push(&index, repo, None, 1).await;
    let b = push(&index, repo, Some(1), 2).await;
    let ttl = Duration::from_mins(1);
    let stale =
        ok(index.acquire_lease(repo, "node-a", ttl).await).unwrap_or_else(|| panic!("lease"));
    let lease =
        ok(index.acquire_lease(repo, "node-a", ttl).await).unwrap_or_else(|| panic!("lease"));
    let out = pending(repo, "geo");
    ok(index.insert_pack(&out).await);
    let inputs = [a.id, b.id];
    let geometric = RepackPublish {
        repo,
        holder: "node-a",
        epoch: lease.epoch,
        outputs: &[out.id],
        inputs: &inputs,
        ref_log_high_water: None,
    };

    let before = ok(index.snapshot(repo).await);
    for bad in [
        RepackPublish {
            epoch: stale.epoch,
            ..geometric
        },
        RepackPublish {
            holder: "node-b",
            ..geometric
        },
    ] {
        match index.publish_repack(bad).await {
            Err(CommitError::Fenced) => {}
            other => panic!("expected Fenced, got {other:?}"),
        }
    }
    assert_eq!(ok(index.snapshot(repo).await), before);

    // A full repack whose working copy predates a push can't publish.
    let high_water = ok(index.ref_log_since(repo, 0).await)
        .last()
        .map_or(0, |r| r.seq);
    let c = push(&index, repo, Some(2), 3).await;
    match index
        .publish_repack(RepackPublish {
            ref_log_high_water: Some(high_water),
            ..geometric
        })
        .await
    {
        Err(CommitError::Fenced) => {}
        other => panic!("expected Fenced, got {other:?}"),
    }
    let unchanged = ok(index.snapshot(repo).await);
    assert_eq!(unchanged.full_repack_gen, 0);
    assert_eq!(unchanged.packs.len(), 3);

    // A geometric repack takes neither fence, only the lease: it publishes.
    ok(index.publish_repack(geometric).await);
    let after = ok(index.snapshot(repo).await);
    assert_eq!(after.full_repack_gen, 0);
    let ids: Vec<PackId> = after.packs.iter().map(|p| p.id).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&c.id) && ids.contains(&out.id));
    // Inputs are superseded, not deleted: a second publish naming them
    // finds them no longer committed, and the outputs no longer pending.
    let out2 = pending(repo, "geo2");
    ok(index.insert_pack(&out2).await);
    match index
        .publish_repack(RepackPublish {
            outputs: &[out2.id],
            ..geometric
        })
        .await
    {
        Err(CommitError::Fenced) => {}
        other => panic!("expected Fenced for superseded inputs, got {other:?}"),
    }
    match index
        .publish_repack(RepackPublish {
            inputs: &[c.id],
            ..geometric
        })
        .await
    {
        Err(CommitError::PackNotPending) => {}
        other => panic!("expected PackNotPending, got {other:?}"),
    }
    assert_eq!(ok(index.snapshot(repo).await), after);
}

#[tokio::test]
async fn unplaced_rows_persist_and_clear_by_coverage() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let repo = new_repo(&index, "a").await;
    let other = new_repo(&index, "b").await;
    let recovered = PackId::generate();
    let committed = PackId::generate();
    let entry = |repo, kind, pack, found_at| Unplaced {
        repo,
        kind,
        pack,
        found_at,
    };
    let rows = [
        entry(repo, UnplacedKind::Pack, recovered, 10),
        entry(repo, UnplacedKind::Commit(oid(1)), recovered, 11),
        entry(repo, UnplacedKind::Tag(oid(2)), committed, 12),
        entry(repo, UnplacedKind::Edge(oid(3)), committed, 13),
        entry(other, UnplacedKind::Commit(oid(1)), recovered, 14),
    ];
    ok(index.record_unplaced(&rows).await);
    // Recording again keeps one entry each.
    ok(index.record_unplaced(&rows[..2]).await);
    assert_eq!(ok(index.unplaced(repo).await), rows[..4]);

    // Durable across a reopen.
    drop(index);
    let index = open(&dir).await;
    assert_eq!(ok(index.unplaced(repo).await), rows[..4]);

    assert_eq!(
        ok(index.clear_unplaced(repo, &[oid(1), oid(2), oid(9)]).await),
        2
    );
    assert_eq!(ok(index.unplaced(repo).await), [rows[0], rows[3]]);
    // Another repository's entries are its own.
    assert_eq!(ok(index.unplaced(other).await), rows[4..]);
    assert_eq!(ok(index.clear_unplaced(repo, &[]).await), 0);
}

/// S9's "Concurrent pushes to one branch": each round, 8 writers on two
/// nodes (index handles on one file) take their snapshot at the same tip,
/// then all commit at once. Exactly one wins, the tip is the winner's,
/// every loser gets `RefMismatch` and marks its pack lost, and the ref log
/// and the committed packs hold one entry per round.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_commits_one_winner() {
    const WRITERS: u8 = 8;
    const ROUNDS: u8 = 20;
    let dir = ok(TempDir::new());
    let nodes = [Arc::new(open(&dir).await), Arc::new(open(&dir).await)];
    let repo = new_repo(&nodes[0], "race").await;
    let mut tip: Option<Oid> = None;
    for round in 0..ROUNDS {
        let barrier = Arc::new(Barrier::new(usize::from(WRITERS)));
        let mut tasks = JoinSet::new();
        for writer in 0..WRITERS {
            let node = Arc::clone(&nodes[usize::from(writer % 2)]);
            let barrier = Arc::clone(&barrier);
            tasks.spawn(async move {
                let snap = ok(node.snapshot(repo).await);
                let old = snap.refs.first().map(|(_, oid)| *oid);
                let new = Oid([round * WRITERS + writer + 1; 20]);
                let pack = pending(repo, &format!("pack-{round}-{writer}"));
                ok(node.insert_pack(&pack).await);
                let updates = [RefUpdate {
                    name: "refs/heads/main".to_owned(),
                    old,
                    new: Some(new),
                }];
                barrier.wait().await;
                match node
                    .commit(tx(repo, &updates, Some(pack.id), snap.full_repack_gen))
                    .await
                {
                    Ok(()) => Some(new),
                    Err(CommitError::RefMismatch(name)) => {
                        assert_eq!(name, "refs/heads/main");
                        assert!(ok(node
                            .mark_pack(pack.id, PackState::Pending, PackState::Lost)
                            .await));
                        None
                    }
                    Err(e) => panic!("round {round} writer {writer}: {e}"),
                }
            });
        }
        let mut winners = Vec::new();
        while let Some(result) = tasks.join_next().await {
            winners.extend(ok(result));
        }
        assert_eq!(winners.len(), 1, "round {round}: {winners:?}");
        let snap = ok(nodes[usize::from(round % 2)].snapshot(repo).await);
        assert_eq!(
            snap.refs,
            vec![("refs/heads/main".to_owned(), winners[0])],
            "round {round}"
        );
        let log = ok(nodes[0].ref_log_since(repo, 0).await);
        assert_eq!(log.len(), usize::from(round) + 1);
        let last = &log[log.len() - 1];
        assert_eq!((last.old, last.new), (tip, Some(winners[0])));
        tip = Some(winners[0]);
    }
    let snap = ok(nodes[1].snapshot(repo).await);
    assert_eq!(snap.packs.len(), usize::from(ROUNDS));
}
