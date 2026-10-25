//! The SQLite index against the `Repos` contract: case-insensitive names
//! (ruling M2-R2), label listings (M2-R8, ADR 0010), per-scope SHA-256
//! lookups (design §5.4), purge (M2-R13) and the append-only audit log
//! (M2-R12).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use openxet_index::{
    AuditEntry, ChunkTable, FileRecord, HubIndex, Index, IndexError, NewFile, Page, Registrant,
    Repo, RepoFilter, RepoId, Repos, SqliteIndex, Term, Visibility,
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

#[tokio::test]
async fn create_then_lookup_by_name_and_id() {
    let dir = ok(TempDir::new());
    let (index, _) = open(&dir).await;
    let mut r = repo("Org", "Model", &["model", "eval"]);
    r.private = true;
    r.full_repack_gen = 3;
    ok(index.create_repo(&r).await);

    assert_eq!(
        ok(index.repo_by_name("Org", "Model").await),
        Some(r.clone())
    );
    assert_eq!(
        ok(index.repo_by_name("org", "MODEL").await),
        Some(r.clone())
    );
    assert_eq!(ok(index.repo_by_id(r.id).await), Some(r.clone()));
    assert_eq!(ok(index.repo_by_name("org", "other").await), None);
    assert_eq!(ok(index.repo_by_id(RepoId::generate()).await), None);

    // Survives a reopen: the row is durable, not cached.
    drop(index);
    let (index, _) = open(&dir).await;
    assert_eq!(ok(index.repo_by_id(r.id).await), Some(r));
}

#[tokio::test]
async fn name_taken_case_insensitively() {
    let dir = ok(TempDir::new());
    let (index, _) = open(&dir).await;
    let first = repo("org", "model", &["model"]);
    ok(index.create_repo(&first).await);

    // Names are unique across labels too (ADR 0010 item 5).
    let clash = repo("Org", "Model", &["dataset"]);
    match index.create_repo(&clash).await {
        Err(IndexError::NameTaken { name }) => assert_eq!(name, "Org/Model"),
        other => panic!("expected NameTaken, got {other:?}"),
    }
    assert_eq!(ok(index.repo_by_id(clash.id).await), None);
    assert_eq!(ok(index.repo_by_name("ORG", "MODEL").await), Some(first));

    // The key joins namespace and name, so neither half collides alone.
    ok(index.create_repo(&repo("org", "model2", &[])).await);
    ok(index.create_repo(&repo("org2", "model", &[])).await);
}

#[tokio::test]
async fn list_filters_by_label_and_paginates() {
    let dir = ok(TempDir::new());
    let (index, path) = open(&dir).await;
    // 1,500 models alternating between two namespaces, written in one
    // transaction as `create_repo` writes them: 1,500 durable commits take
    // half a minute on macOS, where every commit is an `F_FULLFSYNC`.
    let mut models = Vec::new();
    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let mut tx = ok(pool.begin().await);
    for i in 0..1_500 {
        let ns = if i % 2 == 0 { "alpha" } else { "Beta" };
        let r = repo(ns, &format!("m{i:04}"), &["model"]);
        ok(sqlx::query(
            "INSERT INTO repos \
             (id, namespace, name, name_key, private, labels, created_at, full_repack_gen) \
             VALUES (?, ?, ?, ?, 0, '[\"model\"]', ?, 0)",
        )
        .bind(&r.id.0[..])
        .bind(&r.namespace)
        .bind(&r.name)
        .bind(format!("{}/{}", r.namespace, r.name).to_ascii_lowercase())
        .bind(r.created_at.cast_signed())
        .execute(&mut *tx)
        .await);
        models.push(r);
    }
    ok(tx.commit().await);
    assert_eq!(
        ok(index.repo_by_name("beta", "M0001").await).as_ref(),
        Some(&models[1]),
        "seeded as create_repo writes"
    );
    // 15 datasets through the API, alternating every hundred.
    for i in (0..1_500).step_by(100) {
        let ns = if (i / 100) % 2 == 0 { "alpha" } else { "Beta" };
        ok(index
            .create_repo(&repo(ns, &format!("d{i:04}"), &["dataset"]))
            .await);
    }
    let key = |r: &Repo| format!("{}/{}", r.namespace, r.name).to_ascii_lowercase();
    models.sort_by_key(key);

    let filter = RepoFilter {
        label: Some("model".to_owned()),
        namespace: None,
    };
    let first = ok(index.list_repos(filter.clone(), None, 1_000).await);
    assert_eq!(first.items.len(), 1_000);
    let cursor = first.next.clone().unwrap_or_else(|| panic!("a next page"));
    let second = ok(index.list_repos(filter.clone(), Some(&cursor), 1_000).await);
    assert_eq!(second.items.len(), 500);
    assert_eq!(second.next, None);
    let listed: Vec<Repo> = first.items.into_iter().chain(second.items).collect();
    assert_eq!(listed, models);

    // Stable: the same cursor answers the same page.
    let again = ok(index.list_repos(filter, Some(&cursor), 1_000).await);
    assert_eq!(again.items, listed[1_000..]);

    // Namespace filter, ignoring case, combined with the label.
    let beta = ok(index
        .list_repos(
            RepoFilter {
                label: Some("dataset".to_owned()),
                namespace: Some("beta".to_owned()),
            },
            None,
            1_000,
        )
        .await);
    let names: Vec<&str> = beta.items.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "d0100", "d0300", "d0500", "d0700", "d0900", "d1100", "d1300"
        ]
    );
    assert!(beta.items.iter().all(|r| r.namespace == "Beta"));

    // No filter lists everything; an exact page leaves no cursor.
    let all = ok(index.list_repos(RepoFilter::default(), None, 1_515).await);
    assert_eq!(all.items.len(), 1_515);
    assert_eq!(all.next, None);
    let empty = ok(index
        .list_repos(
            RepoFilter {
                label: Some("space".to_owned()),
                namespace: None,
            },
            None,
            10,
        )
        .await);
    assert_eq!(
        empty,
        Page {
            items: vec![],
            next: None
        }
    );
}

#[tokio::test]
async fn set_labels_replaces() {
    let dir = ok(TempDir::new());
    let (index, _) = open(&dir).await;
    let r = repo("org", "thing", &["model"]);
    ok(index.create_repo(&r).await);

    let labels = vec!["dataset".to_owned(), "eval".to_owned()];
    ok(index.set_labels(r.id, &r.labels, &labels).await);
    let got = ok(index.repo_by_id(r.id).await).unwrap_or_else(|| panic!("repo"));
    assert_eq!(got.labels, labels);
    let as_model = ok(index
        .list_repos(
            RepoFilter {
                label: Some("model".to_owned()),
                namespace: None,
            },
            None,
            10,
        )
        .await);
    assert!(as_model.items.is_empty());

    ok(index.set_labels(r.id, &labels, &[]).await);
    let got = ok(index.repo_by_id(r.id).await).unwrap_or_else(|| panic!("repo"));
    assert!(got.labels.is_empty());

    let missing = RepoId::generate();
    match index.set_labels(missing, &[], &labels).await {
        Err(IndexError::UnknownRepo { repo }) => assert_eq!(repo, missing),
        other => panic!("expected UnknownRepo, got {other:?}"),
    }
}

/// Two writers that read the same labels can't both change them: the
/// second one's expected list is stale, and it changes nothing (ruling
/// M2-R12).
#[tokio::test]
async fn set_labels_compares_and_swaps() {
    let dir = ok(TempDir::new());
    let (index, _) = open(&dir).await;
    let r = repo("org", "thing", &["model"]);
    ok(index.create_repo(&r).await);

    let first = vec!["model".to_owned(), "dataset".to_owned()];
    let second = vec!["model".to_owned(), "space".to_owned()];
    ok(index.set_labels(r.id, &r.labels, &first).await);
    match index.set_labels(r.id, &r.labels, &second).await {
        Err(IndexError::LabelsChanged { repo }) => assert_eq!(repo, r.id),
        other => panic!("expected LabelsChanged, got {other:?}"),
    }
    let got = ok(index.repo_by_id(r.id).await).unwrap_or_else(|| panic!("repo"));
    assert_eq!(got.labels, first);
    // Order is part of the list.
    let reordered = vec!["dataset".to_owned(), "model".to_owned()];
    assert!(matches!(
        index.set_labels(r.id, &reordered, &second).await,
        Err(IndexError::LabelsChanged { .. })
    ));
}

/// A table of `n` chunks, each 1000 bytes uncompressed and 600 stored.
fn table(label: &str, n: u32) -> ChunkTable {
    let hashes: Vec<(MerkleHash, u64)> = (0..n)
        .map(|i| (h(&format!("{label}/chunk{i}")), 1000))
        .collect();
    ChunkTable {
        hashes: hashes.into(),
        offsets: (0..=n).map(|i| i * 600).collect::<Vec<_>>().into(),
    }
}

/// Stores xorb `label` of `n` chunks for `tenant`, every chunk eligible.
async fn put(index: &SqliteIndex, label: &str, n: u32, tenant: &str) -> (MerkleHash, ChunkTable) {
    let xorb = h(&format!("xorb/{label}"));
    let t = table(label, n);
    let eligible: Vec<MerkleHash> = t.hashes.iter().map(|(c, _)| *c).collect();
    ok(index
        .put_xorb(&xorb, u64::from(n) * 600, &t, &eligible, tenant)
        .await);
    (xorb, t)
}

/// A file over chunks `start..end` of `xorb`, as a real shard names it:
/// its first chunk is the first term's first chunk.
fn file(label: &str, xorb: MerkleHash, t: &ChunkTable, start: u32, end: u32) -> NewFile {
    NewFile {
        record: FileRecord {
            hash: h(&format!("file/{label}")),
            size: u64::from(end - start) * 1000,
            terms: vec![Term {
                xorb,
                chunk_start: start,
                chunk_end: end,
                unpacked_bytes: (end - start) * 1000,
            }],
        },
        sha256: Some(sha(label)),
        first_chunk: Some(t.hashes[start as usize].0),
    }
}

/// A distinct SHA-256 per label; the index never checks it (design §5.4).
fn sha(label: &str) -> [u8; 32] {
    let mut out = [0; 32];
    out.copy_from_slice(h(&format!("sha256/{label}")).as_bytes());
    out
}

async fn register(index: &SqliteIndex, shard: &str, files: &[NewFile], scope: &str, seq: u64) {
    ok(index
        .register_shard(
            &h(shard),
            files,
            Registrant {
                scope,
                tenant: scope,
                seq,
            },
            Visibility::Global,
        )
        .await);
}

#[tokio::test]
async fn file_by_sha256_is_per_scope() {
    let dir = ok(TempDir::new());
    let (index, _) = open(&dir).await;
    let (xorb, t) = put(&index, "x", 4, "repo:a").await;
    let f = file("same", xorb, &t, 0, 4);
    let sha = f.sha256.unwrap_or_else(|| panic!("sha"));
    register(&index, "shard", std::slice::from_ref(&f), "repo:a", 1).await;

    assert_eq!(
        ok(index.file_by_sha256("repo:a", &sha).await),
        Some(f.record.clone())
    );
    // Client-asserted, so never trusted outside the scope that registered
    // it: repo B must upload the file itself (Review Focus 1).
    assert_eq!(ok(index.file_by_sha256("repo:b", &sha).await), None);
    assert_eq!(ok(index.file_by_sha256("repo:a", &[9; 32]).await), None);

    // Of two files claiming one SHA-256 in a scope, the earliest
    // registration answers, however the later one sorts.
    let mut liar = file("liar", xorb, &t, 0, 2);
    liar.sha256 = Some(sha);
    register(&index, "shard2", std::slice::from_ref(&liar), "repo:a", 2).await;
    assert_eq!(
        ok(index.file_by_sha256("repo:a", &sha).await),
        Some(f.record)
    );
}

/// Every chunk of `t`'s dedup answer, in order.
async fn dedup_answers(index: &SqliteIndex, t: &ChunkTable) -> Vec<Option<MerkleHash>> {
    let mut out = Vec::new();
    for (chunk, _) in t.hashes.iter() {
        out.push(ok(index.dedup_xorb(chunk, "t", Visibility::Global).await));
    }
    out
}

#[tokio::test]
async fn purge_removes_every_scope_row() {
    let dir = ok(TempDir::new());
    let (index, path) = open(&dir).await;
    let (xorb, t) = put(&index, "secret", 6, "t").await;
    let (other_xorb, other_t) = put(&index, "other", 3, "t").await;
    let secret = file("secret", xorb, &t, 1, 4);
    let other = file("other", other_xorb, &other_t, 0, 3);
    register(&index, "s1", std::slice::from_ref(&secret), "repo:a", 1).await;
    register(&index, "s2", &[secret.clone(), other.clone()], "repo:b", 2).await;
    let dedup_before = (
        dedup_answers(&index, &t).await,
        dedup_answers(&index, &other_t).await,
    );
    assert!(dedup_before.0.iter().all(|x| *x == Some(xorb)));
    // What `openxet purge --dry-run` reports, without changing anything.
    assert_eq!(ok(index.file_scope_count(&secret.record.hash).await), 2);

    assert_eq!(ok(index.purge_file(&secret.record.hash).await), 2);
    assert_eq!(ok(index.file_scope_count(&secret.record.hash).await), 0);
    assert_eq!(ok(index.file_scope_count(&other.record.hash).await), 1);

    for scope in ["repo:a", "repo:b"] {
        assert_eq!(ok(index.file(&secret.record.hash, scope).await), None);
    }
    assert_eq!(
        ok(index.file(&other.record.hash, "repo:b").await),
        Some(other.record)
    );
    // Dedup rows describe xorbs, not files, and a rebuild from the bucket
    // restores them, so purge leaves them as they were.
    assert_eq!(
        (
            dedup_answers(&index, &t).await,
            dedup_answers(&index, &other_t).await
        ),
        dedup_before
    );

    // Recorded, and repeating it removes nothing more.
    assert_eq!(ok(index.purge_file(&secret.record.hash).await), 0);
    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let purged: Vec<Vec<u8>> = ok(sqlx::query_scalar("SELECT file FROM purges")
        .fetch_all(&pool)
        .await);
    assert_eq!(purged, vec![secret.record.hash.as_bytes().to_vec()]);
}

/// Later tasks hold the index as one trait object.
#[tokio::test]
async fn sqlite_index_is_a_hub_index() {
    let dir = ok(TempDir::new());
    let (index, _) = open(&dir).await;
    let hub: Arc<dyn HubIndex> = Arc::new(index);
    let r = repo("org", "model", &["model"]);
    ok(hub.create_repo(&r).await);
    assert_eq!(ok(hub.snapshot(r.id).await).full_repack_gen, 0);
    assert_eq!(ok(hub.file(&h("absent"), "repo:x").await), None);
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
