//! The SQLite index against the `Index` contract: idempotency, scope
//! visibility (design §5.4, §5.9 req 4), atomic shard registration and
//! durability across a reopen.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use openxet_index::{
    ChunkTable, FAULT_AFTER_FIRST_FILE_SCOPE, FileRecord, Index, IndexError, NewFile, SqliteIndex,
    Term, Visibility,
};
use openxet_protocol::{MerkleHash, chunk_hash, hash_to_hex};
use tempfile::TempDir;

const A: &str = "model/org-a/repo";
const B: &str = "model/org-b/repo";

fn h(label: &str) -> MerkleHash {
    chunk_hash(label.as_bytes())
}

/// A table of `n` chunks, each 1000 bytes uncompressed and 600 stored.
fn table(label: &str, n: u32) -> ChunkTable {
    let hashes: Vec<(MerkleHash, u64)> = (0..n)
        .map(|i| (h(&format!("{label}/chunk{i}")), 1000))
        .collect();
    let offsets: Vec<u32> = (0..=n).map(|i| i * 600).collect();
    ChunkTable {
        hashes: hashes.into(),
        offsets: offsets.into(),
    }
}

fn file(label: &str, xorb: MerkleHash, chunks: u32) -> NewFile {
    NewFile {
        record: FileRecord {
            hash: h(&format!("file/{label}")),
            size: u64::from(chunks) * 1000,
            terms: vec![Term {
                xorb,
                chunk_start: 0,
                chunk_end: chunks,
                unpacked_bytes: chunks * 1000,
            }],
        },
        sha256: Some([7; 32]),
        first_chunk: Some(h(&format!("first/{label}"))),
    }
}

async fn open(dir: &TempDir) -> SqliteIndex {
    open_at(&dir.path().join("index.sqlite")).await
}

async fn open_at(path: &Path) -> SqliteIndex {
    match SqliteIndex::open(path).await {
        Ok(index) => index,
        Err(e) => panic!("open index: {e}"),
    }
}

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("index call failed: {e}"),
    }
}

/// Stores xorb `label` with `n` chunks for `scope`; every chunk is eligible.
async fn put(index: &dyn Index, label: &str, n: u32, scope: &str) -> (MerkleHash, ChunkTable) {
    let xorb = h(&format!("xorb/{label}"));
    let t = table(label, n);
    let eligible: Vec<MerkleHash> = t.hashes.iter().map(|(c, _)| *c).collect();
    ok(index
        .put_xorb(&xorb, u64::from(n) * 600 + 100, &t, &eligible, scope)
        .await);
    (xorb, t)
}

#[tokio::test]
async fn put_xorb_is_idempotent() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let xorb = h("xorb");
    let t = table("x", 3);
    assert!(ok(index.put_xorb(&xorb, 1900, &t, &[], A).await));

    let other = table("other", 5);
    assert!(!ok(index.put_xorb(&xorb, 9999, &other, &[], A).await));

    let tables = ok(index.chunk_tables(&[xorb], A, Visibility::PerScope).await);
    assert_eq!(tables.get(&xorb), Some(&t));
    assert_eq!(
        ok(index.xorb_size(&xorb, A, Visibility::PerScope).await),
        Some(1900)
    );
}

#[tokio::test]
async fn xorb_visible_per_scope_only_when_per_scope() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xorb, t) = put(&index, "x", 2, A).await;

    assert_eq!(
        ok(index.xorb_size(&xorb, B, Visibility::Global).await),
        Some(1300)
    );
    assert_eq!(
        ok(index.xorb_size(&xorb, B, Visibility::PerScope).await),
        None
    );
    assert_eq!(
        ok(index.xorb_size(&xorb, A, Visibility::PerScope).await),
        Some(1300)
    );

    // Uploading the same xorb to B is new for B and makes it visible there.
    assert!(ok(index.put_xorb(&xorb, 1300, &t, &[], B).await));
    assert_eq!(
        ok(index.xorb_size(&xorb, B, Visibility::PerScope).await),
        Some(1300)
    );
}

#[tokio::test]
async fn register_shard_is_idempotent_per_scope() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xorb, _) = put(&index, "x", 4, A).await;
    let shard = h("shard");
    let files = [file("f1", xorb, 2), file("f2", xorb, 4)];

    assert!(ok(index
        .register_shard(&shard, &files, A, Visibility::Global)
        .await));
    assert!(!ok(index
        .register_shard(&shard, &files, A, Visibility::Global)
        .await));
    assert!(ok(index
        .register_shard(&shard, &files, B, Visibility::Global)
        .await));
    assert!(!ok(index
        .register_shard(&shard, &files, B, Visibility::Global)
        .await));

    // A new shard naming already-registered files is still new for the scope.
    assert!(ok(index
        .register_shard(&h("shard2"), &files, A, Visibility::Global)
        .await));
}

#[tokio::test]
async fn file_only_readable_in_registered_scope() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xorb, _) = put(&index, "x", 3, A).await;
    let f = file("f", xorb, 3);
    ok(index
        .register_shard(
            &h("shard"),
            std::slice::from_ref(&f),
            A,
            Visibility::PerScope,
        )
        .await);

    assert_eq!(
        ok(index.file(&f.record.hash, A).await),
        Some(f.record.clone())
    );
    assert_eq!(ok(index.file(&f.record.hash, B).await), None);
    assert_eq!(ok(index.file(&h("file/unknown"), A).await), None);
}

#[tokio::test]
async fn same_file_two_scopes_keeps_each_scopes_terms() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xa, _) = put(&index, "a", 2, A).await;
    let (xb, _) = put(&index, "b", 2, B).await;
    let fa = file("f", xa, 2);
    let mut fb = file("f", xb, 2);
    fb.first_chunk = Some(h("first/f-in-b"));
    assert_eq!(fa.record.hash, fb.record.hash);
    ok(index
        .register_shard(
            &h("shard-a"),
            std::slice::from_ref(&fa),
            A,
            Visibility::PerScope,
        )
        .await);
    ok(index
        .register_shard(
            &h("shard-b"),
            std::slice::from_ref(&fb),
            B,
            Visibility::PerScope,
        )
        .await);

    assert_eq!(
        ok(index.file(&fa.record.hash, A).await),
        Some(fa.record.clone())
    );
    assert_eq!(
        ok(index.file(&fb.record.hash, B).await),
        Some(fb.record.clone())
    );

    // A later shard in A with other terms keeps A's first registration.
    ok(index
        .register_shard(
            &h("shard-a2"),
            std::slice::from_ref(&fb),
            A,
            Visibility::Global,
        )
        .await);
    assert_eq!(ok(index.file(&fa.record.hash, A).await), Some(fa.record));
}

#[tokio::test]
async fn register_shard_rejects_invisible_xorb_without_revealing_existence() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (foreign, _) = put(&index, "foreign", 2, A).await;
    let (own, _) = put(&index, "own", 2, B).await;
    let missing = h("xorb/missing");

    let unknown = |r: Result<bool, IndexError>| match r {
        Err(IndexError::UnknownXorb { xorb }) => xorb,
        other => panic!("expected UnknownXorb, got {other:?}"),
    };
    // Under PerScope a stored-but-invisible xorb answers exactly like one
    // that isn't stored, so registration can't probe other scopes.
    let mut answers = Vec::new();
    for xorb in [foreign, missing] {
        let mut f = file("f", own, 2);
        f.record.terms.push(Term {
            xorb,
            chunk_start: 0,
            chunk_end: 1,
            unpacked_bytes: 1000,
        });
        let err = index
            .register_shard(
                &h("shard"),
                std::slice::from_ref(&f),
                B,
                Visibility::PerScope,
            )
            .await;
        let message = match &err {
            Err(e) => e.to_string().replace(&hash_to_hex(&xorb), "<xorb>"),
            Ok(_) => String::new(),
        };
        assert_eq!(unknown(err), xorb);
        answers.push(message);
        assert_eq!(ok(index.file(&f.record.hash, B).await), None);
    }
    assert_eq!(answers[0], answers[1]);

    // Under Global, another scope's xorb is usable and only a missing one fails.
    let over_foreign = file("g", foreign, 2);
    assert!(ok(index
        .register_shard(&h("shard"), &[over_foreign], B, Visibility::Global)
        .await));
    let over_missing = file("m", missing, 1);
    assert_eq!(
        unknown(
            index
                .register_shard(&h("shard-m"), &[over_missing], B, Visibility::Global)
                .await
        ),
        missing
    );
}

#[tokio::test]
async fn sha256_mapping_is_scope_local() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let index = open_at(&path).await;
    let (xorb, _) = put(&index, "x", 1, A).await;
    let mut fa = file("f", xorb, 1);
    fa.sha256 = Some([1; 32]);
    let mut fb = fa.clone();
    fb.sha256 = Some([2; 32]);
    ok(index
        .register_shard(
            &h("shard-a"),
            std::slice::from_ref(&fa),
            A,
            Visibility::Global,
        )
        .await);
    ok(index
        .register_shard(&h("shard-b"), &[fb], B, Visibility::Global)
        .await);
    drop(index);

    // The lookup API arrives in M2; until then the column is checked directly.
    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let rows: Vec<(String, Vec<u8>)> = ok(sqlx::query_as(
        "SELECT scope, sha256 FROM file_scopes WHERE file = ? ORDER BY scope",
    )
    .bind(fa.record.hash.as_bytes())
    .fetch_all(&pool)
    .await);
    assert_eq!(
        rows,
        vec![(A.to_owned(), vec![1; 32]), (B.to_owned(), vec![2; 32])]
    );
}

#[tokio::test]
async fn dedup_xorb_prefers_newest_and_respects_visibility() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let shared = h("shared-chunk");
    let t = table("t", 1);
    let (old, new) = (h("xorb/old"), h("xorb/new"));
    ok(index.put_xorb(&old, 700, &t, &[shared], A).await);
    ok(index.put_xorb(&new, 700, &t, &[shared], B).await);

    assert_eq!(
        ok(index.dedup_xorb(&shared, A, Visibility::Global).await),
        Some(new)
    );
    assert_eq!(
        ok(index.dedup_xorb(&shared, A, Visibility::PerScope).await),
        Some(old)
    );
    assert_eq!(
        ok(index.dedup_xorb(&shared, B, Visibility::PerScope).await),
        Some(new)
    );
    assert_eq!(
        ok(index
            .dedup_xorb(&shared, "other", Visibility::PerScope)
            .await),
        None
    );

    // Re-uploading the old xorb doesn't make it newer.
    ok(index.put_xorb(&old, 700, &t, &[shared], B).await);
    assert_eq!(
        ok(index.dedup_xorb(&shared, B, Visibility::PerScope).await),
        Some(new)
    );

    // Chunks not named eligible are never dedup answers.
    assert_eq!(
        ok(index
            .dedup_xorb(&t.hashes[0].0, A, Visibility::Global)
            .await),
        None
    );

    // A registered file's first chunk is answered with its first xorb.
    let f = file("f", old, 1);
    let first = f.first_chunk;
    ok(index
        .register_shard(&h("shard"), &[f], A, Visibility::PerScope)
        .await);
    assert_eq!(
        ok(index
            .dedup_xorb(&first.unwrap_or_default(), A, Visibility::PerScope)
            .await),
        Some(old)
    );
}

#[tokio::test]
async fn chunk_tables_returns_only_present_visible() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xa, ta) = put(&index, "a", 2, A).await;
    let (xb, tb) = put(&index, "b", 3, B).await;
    let missing = h("xorb/missing");
    let asked = [xa, xb, missing, xa];

    let per_scope = ok(index.chunk_tables(&asked, A, Visibility::PerScope).await);
    assert_eq!(per_scope, HashMap::from([(xa, ta.clone())]));

    let global = ok(index.chunk_tables(&asked, A, Visibility::Global).await);
    assert_eq!(global, HashMap::from([(xa, ta), (xb, tb)]));

    assert!(ok(index.chunk_tables(&[], A, Visibility::Global).await).is_empty());
}

#[tokio::test]
async fn chunk_tables_handles_more_xorbs_than_one_query_binds() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let mut asked = Vec::new();
    for i in 0..1200 {
        asked.push(put(&index, &format!("x{i}"), 1, A).await.0);
    }
    let tables = ok(index.chunk_tables(&asked, A, Visibility::PerScope).await);
    assert_eq!(tables.len(), 1200);
}

#[tokio::test]
async fn register_shard_is_atomic() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xorb, _) = put(&index, "x", 2, FAULT_AFTER_FIRST_FILE_SCOPE).await;
    let shard = h("shard");
    let files = [file("f1", xorb, 1), file("f2", xorb, 2)];

    assert!(
        index
            .register_shard(
                &shard,
                &files,
                FAULT_AFTER_FIRST_FILE_SCOPE,
                Visibility::Global
            )
            .await
            .is_err()
    );
    for f in &files {
        assert_eq!(
            ok(index
                .file(&f.record.hash, FAULT_AFTER_FIRST_FILE_SCOPE)
                .await),
            None
        );
        let first = f.first_chunk.unwrap_or_default();
        assert_eq!(
            ok(index.dedup_xorb(&first, A, Visibility::Global).await),
            None
        );
    }
    assert_eq!(
        ok(index.dedup_shard_xorbs(&xorb, A, Visibility::Global).await),
        None,
        "the shard_xorbs rows rolled back"
    );
    // The shard_scopes row rolled back too: registering the shard again in
    // the same scope is new. No files means the fault point isn't reached.
    assert!(ok(index
        .register_shard(
            &shard,
            &[],
            FAULT_AFTER_FIRST_FILE_SCOPE,
            Visibility::Global
        )
        .await));
    assert!(!ok(index
        .register_shard(
            &shard,
            &[],
            FAULT_AFTER_FIRST_FILE_SCOPE,
            Visibility::Global
        )
        .await));
}

#[tokio::test]
async fn reopen_keeps_everything() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let index = open_at(&path).await;
    let (xorb, t) = put(&index, "x", 3, A).await;
    let f = file("f", xorb, 3);
    ok(index
        .register_shard(
            &h("shard"),
            std::slice::from_ref(&f),
            A,
            Visibility::PerScope,
        )
        .await);
    let first = t.hashes[0].0;
    drop(index);

    let index: Arc<dyn Index> = Arc::new(open_at(&path).await);
    assert_eq!(
        ok(index.xorb_size(&xorb, A, Visibility::PerScope).await),
        Some(1900)
    );
    assert_eq!(
        ok(index.chunk_tables(&[xorb], A, Visibility::PerScope).await),
        HashMap::from([(xorb, t)])
    );
    assert_eq!(
        ok(index.file(&f.record.hash, A).await),
        Some(f.record.clone())
    );
    assert_eq!(
        ok(index.dedup_xorb(&first, A, Visibility::PerScope).await),
        Some(xorb)
    );
    assert!(!ok(index
        .register_shard(&h("shard"), &[f], A, Visibility::PerScope)
        .await));
}

#[tokio::test]
async fn put_xorb_rejects_malformed_table() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let mut t = table("x", 2);
    t.offsets = vec![0, 600].into();
    assert!(index.put_xorb(&h("xorb"), 1, &t, &[], A).await.is_err());
    assert_eq!(
        ok(index.xorb_size(&h("xorb"), A, Visibility::Global).await),
        None
    );
}

/// `put_xorb` derives the next `ingested_seq` from the current maximum
/// under the write lock; it must be an index lookup, not a table scan.
#[tokio::test]
async fn next_ingested_seq_is_an_index_lookup() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    drop(open_at(&path).await);
    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let plan: Vec<(i64, i64, i64, String)> = ok(sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT COALESCE(MAX(ingested_seq), 0) + 1 FROM xorbs WHERE true",
    )
    .fetch_all(&pool)
    .await);
    let details: Vec<&str> = plan.iter().map(|row| row.3.as_str()).collect();
    // Without an index SQLite still reports `SEARCH xorbs`, but it visits
    // every row; only a covering index makes MAX a single seek.
    assert!(
        details.iter().any(|d| d.contains("USING COVERING INDEX")),
        "plan visits every xorb: {details:?}"
    );
}

/// A database error keeps SQLite's code and message as its `source()`, so
/// logs that walk the chain say why the index failed, not only what to try.
#[tokio::test]
async fn database_errors_carry_the_sqlite_cause() {
    let dir = ok(TempDir::new());
    let missing_dir = dir.path().join("missing").join("index.sqlite");
    let Err(err) = SqliteIndex::open(&missing_dir).await else {
        panic!("opened an index in a directory that doesn't exist");
    };
    assert!(matches!(err, IndexError::Database(_)), "got {err:?}");
    let cause = std::error::Error::source(&err)
        .map(ToString::to_string)
        .unwrap_or_default();
    assert!(cause.contains("(code: 14)"), "cause was {cause:?}");
}

/// A file of one term per `(xorb, chunks)`, in order.
fn file_over(label: &str, terms: &[(MerkleHash, u32)]) -> NewFile {
    let terms: Vec<Term> = terms
        .iter()
        .map(|(xorb, chunks)| Term {
            xorb: *xorb,
            chunk_start: 0,
            chunk_end: *chunks,
            unpacked_bytes: chunks * 1000,
        })
        .collect();
    NewFile {
        record: FileRecord {
            hash: h(&format!("file/{label}")),
            size: terms.iter().map(|t| u64::from(t.unpacked_bytes)).sum(),
            terms,
        },
        sha256: None,
        first_chunk: Some(h(&format!("first/{label}"))),
    }
}

#[tokio::test]
async fn dedup_shard_xorbs_lists_the_shards_xorbs_in_term_order() {
    // Ruling M1-R30: the order of first reference across the shard's files'
    // terms, which is `UploadShard::referenced_xorbs` order.
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x1, _) = put(&index, "x1", 2, A).await;
    let (x2, _) = put(&index, "x2", 2, A).await;
    let (x3, _) = put(&index, "x3", 2, A).await;
    let (x4, _) = put(&index, "x4", 2, A).await;
    let files = [
        file_over("f1", &[(x2, 1), (x1, 2), (x2, 2)]),
        file_over("f2", &[(x3, 1), (x1, 1), (x4, 2)]),
    ];
    ok(index
        .register_shard(&h("shard"), &files, A, Visibility::PerScope)
        .await);

    for xorb in [x1, x2, x3, x4] {
        for vis in [Visibility::Global, Visibility::PerScope] {
            assert_eq!(
                ok(index.dedup_shard_xorbs(&xorb, A, vis).await),
                Some(vec![x2, x1, x3, x4])
            );
        }
    }
}

#[tokio::test]
async fn dedup_shard_xorbs_none_without_a_shard() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x1, _) = put(&index, "x1", 2, A).await;
    let (x2, _) = put(&index, "x2", 2, A).await;
    ok(index
        .register_shard(
            &h("shard"),
            &[file_over("f", &[(x2, 2)])],
            A,
            Visibility::Global,
        )
        .await);

    for vis in [Visibility::Global, Visibility::PerScope] {
        assert_eq!(ok(index.dedup_shard_xorbs(&x1, A, vis).await), None);
        assert_eq!(ok(index.dedup_shard_xorbs(&h("never"), A, vis).await), None);
    }
}

#[tokio::test]
async fn dedup_shard_xorbs_prefers_the_newest_shard() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x1, _) = put(&index, "x1", 2, A).await;
    let (x2, _) = put(&index, "x2", 2, A).await;
    let (x3, _) = put(&index, "x3", 2, A).await;
    let old = [file_over("old", &[(x1, 2), (x2, 2)])];
    let new = [file_over("new", &[(x3, 2), (x1, 2)])];
    ok(index
        .register_shard(&h("old"), &old, A, Visibility::PerScope)
        .await);
    ok(index
        .register_shard(&h("new"), &new, A, Visibility::PerScope)
        .await);
    // Registering the older shard again changes nothing, so it stays older.
    assert!(!ok(index
        .register_shard(&h("old"), &old, A, Visibility::PerScope)
        .await));

    for vis in [Visibility::Global, Visibility::PerScope] {
        assert_eq!(
            ok(index.dedup_shard_xorbs(&x1, A, vis).await),
            Some(vec![x3, x1])
        );
        assert_eq!(
            ok(index.dedup_shard_xorbs(&x2, A, vis).await),
            Some(vec![x1, x2])
        );
    }

    // Under `PerScope` only the caller's registrations count: B registers
    // the old shard later, which makes it the newest for B and, under
    // `Global`, for everyone, but not for A under `PerScope`.
    for x in [x1, x2] {
        ok(index
            .put_xorb(&x, 1300, &table(&format!("{x}"), 2), &[], B)
            .await);
    }
    ok(index
        .register_shard(&h("old"), &old, B, Visibility::PerScope)
        .await);
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, B, Visibility::PerScope).await),
        Some(vec![x1, x2])
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::Global).await),
        Some(vec![x1, x2])
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::PerScope).await),
        Some(vec![x3, x1])
    );
}

#[tokio::test]
async fn dedup_shard_xorbs_respects_scope_and_visibility() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x1, _) = put(&index, "x1", 2, A).await;
    let (x2, _) = put(&index, "x2", 2, B).await;

    // Another scope's shard is never offered under `PerScope`, even when
    // the caller holds the xorb itself.
    ok(index
        .register_shard(
            &h("a-only"),
            &[file_over("a", &[(x1, 2)])],
            A,
            Visibility::PerScope,
        )
        .await);
    ok(index.put_xorb(&x1, 1300, &table("x1", 2), &[], B).await);
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, B, Visibility::PerScope).await),
        None
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, B, Visibility::Global).await),
        Some(vec![x1])
    );

    // A shard registered under `Global` may mix xorbs of several scopes;
    // read under `PerScope` it lists only the caller's.
    ok(index
        .register_shard(
            &h("mixed"),
            &[file_over("m", &[(x2, 2), (x1, 2)])],
            A,
            Visibility::Global,
        )
        .await);
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::PerScope).await),
        Some(vec![x1])
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::Global).await),
        Some(vec![x2, x1])
    );
}

/// `register_shard` derives the next `registered_seq` from the current
/// maximum under the write lock, like `ingested_seq`.
#[tokio::test]
async fn next_registered_seq_is_an_index_lookup() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    drop(open_at(&path).await);
    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let plan: Vec<(i64, i64, i64, String)> = ok(sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT COALESCE(MAX(registered_seq), 0) + 1 FROM shard_scopes WHERE true",
    )
    .fetch_all(&pool)
    .await);
    let details: Vec<&str> = plan.iter().map(|row| row.3.as_str()).collect();
    assert!(
        details.iter().any(|d| d.contains("USING COVERING INDEX")),
        "plan visits every shard registration: {details:?}"
    );
}
