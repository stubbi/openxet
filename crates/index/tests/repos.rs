//! What only the SQLite index shows of the `Repos` contract, in its rows:
//! purge records (M2-R13) and the append-only audit log (M2-R12). The
//! contract itself is the conformance suite's (`sqlite_conformance.rs`).

use std::collections::HashSet;
use std::path::PathBuf;

use openxet_index::{
    AuditEntry, ChunkTable, FileRecord, Index, NewFile, Registrant, Repo, RepoId, Repos,
    SqliteIndex, Term, Visibility,
};
use openxet_protocol::{MerkleHash, chunk_hash};
use tempfile::TempDir;

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("index call failed: {e}"),
    }
}

async fn open(dir: &TempDir) -> (SqliteIndex, PathBuf) {
    let path = dir.path().join("index.sqlite");
    (ok(SqliteIndex::open(&path).await), path)
}

fn repo(namespace: &str, name: &str, labels: &[&str]) -> Repo {
    Repo {
        id: RepoId::generate(),
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        private: false,
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        created_at: 1_761_400_000,
        full_repack_gen: 0,
    }
}

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

/// A purge is recorded once, so the purge can be replayed after a
/// rebuild; repeating it records nothing more.
#[tokio::test]
async fn purge_is_recorded_once() {
    let dir = ok(TempDir::new());
    let (index, path) = open(&dir).await;
    let xorb = h("xorb/secret");
    let table = ChunkTable {
        hashes: vec![(h("secret/chunk0"), 1000)].into(),
        offsets: vec![0, 600].into(),
    };
    ok(index.put_xorb(&xorb, 600, &table, &[], "t").await);
    let secret = NewFile {
        record: FileRecord {
            hash: h("file/secret"),
            size: 1000,
            terms: vec![Term {
                xorb,
                chunk_start: 0,
                chunk_end: 1,
                unpacked_bytes: 1000,
            }],
        },
        sha256: None,
        first_chunk: None,
    };
    ok(index
        .register_shard(
            &h("s1"),
            std::slice::from_ref(&secret),
            Registrant {
                scope: "repo:a",
                tenant: "repo:a",
                seq: 1,
            },
            Visibility::Global,
        )
        .await);

    assert_eq!(ok(index.purge_file(&secret.record.hash).await), 1);
    assert_eq!(ok(index.purge_file(&secret.record.hash).await), 0);
    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let purged: Vec<Vec<u8>> = ok(sqlx::query_scalar("SELECT file FROM purges")
        .fetch_all(&pool)
        .await);
    assert_eq!(purged, vec![secret.record.hash.as_bytes().to_vec()]);
}

/// `audit_log`'s `(seq, actor, action, repo, detail, at)`.
type AuditRow = (i64, String, String, Option<Vec<u8>>, String, i64);

#[tokio::test]
async fn audit_rows_are_append_only() {
    let dir = ok(TempDir::new());
    let (index, path) = open(&dir).await;
    let r = repo("org", "model", &["model"]);
    let entry = AuditEntry {
        actor: "alice".to_owned(),
        action: "label.add".to_owned(),
        repo: Some(r.id),
        detail: serde_json::json!({"label": "eval"}),
    };
    // Identical entries are two events, both kept.
    ok(index.audit(&entry).await);
    ok(index.audit(&entry).await);
    ok(index
        .audit(&AuditEntry {
            actor: "operator".to_owned(),
            action: "purge".to_owned(),
            repo: None,
            detail: serde_json::json!({"file": "ab"}),
        })
        .await);

    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let rows: Vec<AuditRow> = ok(sqlx::query_as(
        "SELECT seq, actor, action, repo, detail, at FROM audit_log ORDER BY seq",
    )
    .fetch_all(&pool)
    .await);
    assert_eq!(rows.len(), 3);
    let seqs: HashSet<i64> = rows.iter().map(|r| r.0).collect();
    assert_eq!(seqs.len(), 3);
    assert_eq!(rows[0].1, "alice");
    assert_eq!(rows[0].3.as_deref(), Some(&r.id.0[..]));
    assert_eq!(
        ok(serde_json::from_str::<serde_json::Value>(&rows[0].4)),
        serde_json::json!({"label": "eval"})
    );
    assert_eq!(rows[2].3, None);
    assert!(rows.iter().all(|r| r.5 > 1_700_000_000), "{rows:?}");

    for change in [
        "UPDATE audit_log SET actor = 'mallory'",
        "DELETE FROM audit_log",
    ] {
        let err = sqlx::query(change).execute(&pool).await;
        assert!(
            err.as_ref()
                .is_err_and(|e| e.to_string().contains("append-only")),
            "{change}: {err:?}"
        );
    }
    let count: i64 = ok(sqlx::query_scalar("SELECT COUNT(*) FROM audit_log")
        .fetch_one(&pool)
        .await);
    assert_eq!(count, 3);
}
