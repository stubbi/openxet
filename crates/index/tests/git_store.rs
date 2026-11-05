//! What only the SQLite index shows of the `GitStore` contract, in its
//! rows: a deleted repository leaves none behind. The contract itself is
//! the conformance suite's (`sqlite_conformance.rs`).

use std::time::Duration;

use openxet_index::{
    CommitTx, GitStore, Oid, Pack, PackId, PackState, RefUpdate, Repo, RepoId, Repos, SqliteIndex,
    Unplaced, UnplacedKind,
};
use tempfile::TempDir;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("index call failed: {e}"),
    }
}

/// Deleting a repository removes its refs, lease and unplaced entries; its
/// packs are handed to GC (the conformance suite's
/// `delete_repo_hands_packs_to_gc`).
#[tokio::test]
async fn delete_repo_leaves_no_rows() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let index = ok(SqliteIndex::open(&path).await);
    let repo = Repo {
        id: RepoId::generate(),
        namespace: "org".to_owned(),
        name: "doomed".to_owned(),
        private: false,
        labels: vec!["model".to_owned()],
        created_at: 1_761_400_000,
        full_repack_gen: 0,
    };
    ok(index.create_repo(&repo).await);
    let pack = Pack {
        id: PackId::generate(),
        repo: repo.id,
        name: "pack-1".to_owned(),
        state: PackState::Pending,
        created_at: 1_761_400_100,
        bytes: 4096,
        midx_gen: None,
    };
    ok(index.insert_pack(&pack).await);
    let updates = [RefUpdate {
        name: "refs/heads/main".to_owned(),
        old: None,
        new: Some(Oid([1; 20])),
    }];
    ok(index
        .commit(CommitTx {
            repo: repo.id,
            updates: &updates,
            pack: Some(pack.id),
            full_repack_gen: 0,
            actor: "alice",
            request: "req-1",
        })
        .await);
    ok(index
        .acquire_lease(repo.id, "node-a", Duration::from_mins(1))
        .await)
    .unwrap_or_else(|| panic!("lease"));
    ok(index
        .record_unplaced(&[Unplaced {
            repo: repo.id,
            kind: UnplacedKind::Commit(Oid([7; 20])),
            pack: pack.id,
            found_at: 1,
        }])
        .await);

    assert!(ok(index.delete_repo(repo.id).await));

    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    for table in ["repos", "refs", "repo_leases", "reindex_unplaced"] {
        let column = if table == "repos" { "id" } else { "repo" };
        let count: i64 = ok(sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT COUNT(*) FROM {table} WHERE {column} = ?"
        )))
        .bind(&repo.id.0[..])
        .fetch_one(&pool)
        .await);
        assert_eq!(count, 0, "{table}");
    }
}
