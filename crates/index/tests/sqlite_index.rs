//! The SQLite index against the `Index` contract: idempotency, tenant
//! visibility (design §5.4, §5.9 req 4), atomic shard registration and
//! durability across a reopen.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use std::sync::atomic::{AtomicU64, Ordering};

use openxet_index::{
    ChunkLayout, ChunkTable, FAULT_AFTER_FIRST_FILE_SCOPE, FileRecord, Index, IndexError, NewFile,
    Registrant, SqliteIndex, Term, Visibility,
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

/// A registration to `scope` as its own tenant, numbered in call order.
fn reg(scope: &str) -> Registrant<'_> {
    static SEQ: AtomicU64 = AtomicU64::new(1);
    Registrant {
        scope,
        tenant: scope,
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
    }
}

fn ok<T, E: std::fmt::Display>(r: Result<T, E>) -> T {
    match r {
        Ok(v) => v,
        Err(e) => panic!("index call failed: {e}"),
    }
}

/// Stores xorb `label` with `n` chunks for `tenant`; every chunk is eligible.
async fn put(index: &dyn Index, label: &str, n: u32, tenant: &str) -> (MerkleHash, ChunkTable) {
    let xorb = h(&format!("xorb/{label}"));
    let t = table(label, n);
    let eligible: Vec<MerkleHash> = t.hashes.iter().map(|(c, _)| *c).collect();
    ok(index
        .put_xorb(&xorb, u64::from(n) * 600 + 100, &t, &eligible, tenant)
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

    let tables = ok(index.chunk_tables(&[xorb], A, Visibility::PerTenant).await);
    assert_eq!(tables.get(&xorb), Some(&t));
    assert_eq!(
        ok(index.xorb_size(&xorb, A, Visibility::PerTenant).await),
        Some(1900)
    );
}

#[tokio::test]
async fn xorb_visible_per_tenant_only_when_per_tenant() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xorb, t) = put(&index, "x", 2, A).await;

    assert_eq!(
        ok(index.xorb_size(&xorb, B, Visibility::Global).await),
        Some(1300)
    );
    assert_eq!(
        ok(index.xorb_size(&xorb, B, Visibility::PerTenant).await),
        None
    );
    assert_eq!(
        ok(index.xorb_size(&xorb, A, Visibility::PerTenant).await),
        Some(1300)
    );

    // Uploading the same xorb to B is new for B and makes it visible there.
    assert!(ok(index.put_xorb(&xorb, 1300, &t, &[], B).await));
    assert_eq!(
        ok(index.xorb_size(&xorb, B, Visibility::PerTenant).await),
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
        .register_shard(&shard, &files, reg(A), Visibility::Global)
        .await));
    assert!(!ok(index
        .register_shard(&shard, &files, reg(A), Visibility::Global)
        .await));
    assert!(ok(index
        .register_shard(&shard, &files, reg(B), Visibility::Global)
        .await));
    assert!(!ok(index
        .register_shard(&shard, &files, reg(B), Visibility::Global)
        .await));

    // A new shard naming already-registered files is still new for the scope.
    assert!(ok(index
        .register_shard(&h("shard2"), &files, reg(A), Visibility::Global)
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
            reg(A),
            Visibility::PerTenant,
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
async fn files_answer_each_scopes_own_registrations_in_one_call() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xa, _) = put(&index, "a", 3, A).await;
    let (xb, _) = put(&index, "b", 2, B).await;
    let fa = file("a", xa, 3);
    let fa2 = file("a2", xa, 1);
    // The same file hash, registered in B over B's own xorb.
    let mut fb = file("a", xb, 2);
    fb.first_chunk = None;
    let only_b = file("only-b", xb, 2);
    let registered = [fa.clone(), fa2.clone()];
    ok(index
        .register_shard(&h("shard-a"), &registered, reg(A), Visibility::PerTenant)
        .await);
    ok(index
        .register_shard(
            &h("shard-b"),
            &[fb.clone(), only_b.clone()],
            reg(B),
            Visibility::PerTenant,
        )
        .await);
    let unknown = h("file/unknown");
    let asked = [
        fa.record.hash,
        unknown,
        only_b.record.hash,
        fa2.record.hash,
        fa.record.hash,
    ];

    assert_eq!(
        ok(index.files(&asked, A).await),
        HashMap::from([
            (fa.record.hash, fa.record.clone()),
            (fa2.record.hash, fa2.record.clone()),
        ])
    );
    assert_eq!(
        ok(index.files(&asked, B).await),
        HashMap::from([
            (fb.record.hash, fb.record.clone()),
            (only_b.record.hash, only_b.record.clone()),
        ])
    );
    assert!(ok(index.files(&[], A).await).is_empty());
}

#[tokio::test]
async fn files_handles_more_files_than_one_query_binds() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xorb, _) = put(&index, "x", 1, A).await;
    let files: Vec<NewFile> = (0..1200).map(|i| file(&format!("f{i}"), xorb, 1)).collect();
    ok(index
        .register_shard(&h("shard"), &files, reg(A), Visibility::PerTenant)
        .await);
    let asked: Vec<MerkleHash> = files.iter().map(|f| f.record.hash).collect();
    let records = ok(index.files(&asked, A).await);
    assert_eq!(records.len(), 1200);
    for f in &files {
        assert_eq!(records.get(&f.record.hash), Some(&f.record));
    }
}

#[tokio::test]
async fn term_count_sums_the_scopes_files_once_each() {
    // Batch reconstruction checks its term cap with this before decoding
    // any file (ruling M1-R40), so it must count exactly what `files`
    // would return.
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xa, _) = put(&index, "a", 8, A).await;
    let (xb, _) = put(&index, "b", 2, B).await;
    let mut three = file("three", xa, 1);
    three.record.terms = (0..3)
        .map(|i| Term {
            xorb: xa,
            chunk_start: i,
            chunk_end: i + 1,
            unpacked_bytes: 1000,
        })
        .collect();
    three.record.size = 3000;
    let one = file("one", xa, 4);
    ok(index
        .register_shard(
            &h("shard-a"),
            &[three.clone(), one.clone()],
            reg(A),
            Visibility::PerTenant,
        )
        .await);
    let other = file("other", xb, 2);
    ok(index
        .register_shard(
            &h("shard-b"),
            std::slice::from_ref(&other),
            reg(B),
            Visibility::PerTenant,
        )
        .await);
    let asked = [
        three.record.hash,
        h("file/unknown"),
        other.record.hash,
        one.record.hash,
        three.record.hash,
    ];

    assert_eq!(ok(index.term_count(&asked, A).await), 4);
    assert_eq!(ok(index.term_count(&asked, B).await), 1);
    assert_eq!(ok(index.term_count(&[], A).await), 0);

    let many: Vec<NewFile> = (0..1200).map(|i| file(&format!("f{i}"), xa, 1)).collect();
    ok(index
        .register_shard(&h("shard-many"), &many, reg(A), Visibility::PerTenant)
        .await);
    let asked: Vec<MerkleHash> = many.iter().map(|f| f.record.hash).collect();
    assert_eq!(ok(index.term_count(&asked, A).await), 1200);
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
            reg(A),
            Visibility::PerTenant,
        )
        .await);
    ok(index
        .register_shard(
            &h("shard-b"),
            std::slice::from_ref(&fb),
            reg(B),
            Visibility::PerTenant,
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
            reg(A),
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
    // Under PerTenant a stored-but-invisible xorb answers exactly like one
    // that isn't stored, so registration can't probe other tenants.
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
                reg(B),
                Visibility::PerTenant,
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

    // Under Global, another tenant's xorb is usable and only a missing one fails.
    let over_foreign = file("g", foreign, 2);
    assert!(ok(index
        .register_shard(&h("shard"), &[over_foreign], reg(B), Visibility::Global)
        .await));
    let over_missing = file("m", missing, 1);
    assert_eq!(
        unknown(
            index
                .register_shard(&h("shard-m"), &[over_missing], reg(B), Visibility::Global)
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
            reg(A),
            Visibility::Global,
        )
        .await);
    ok(index
        .register_shard(&h("shard-b"), &[fb], reg(B), Visibility::Global)
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
        ok(index.dedup_xorb(&shared, A, Visibility::PerTenant).await),
        Some(old)
    );
    assert_eq!(
        ok(index.dedup_xorb(&shared, B, Visibility::PerTenant).await),
        Some(new)
    );
    assert_eq!(
        ok(index
            .dedup_xorb(&shared, "other", Visibility::PerTenant)
            .await),
        None
    );

    // Re-uploading the old xorb doesn't make it newer.
    ok(index.put_xorb(&old, 700, &t, &[shared], B).await);
    assert_eq!(
        ok(index.dedup_xorb(&shared, B, Visibility::PerTenant).await),
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
        .register_shard(&h("shard"), &[f], reg(A), Visibility::PerTenant)
        .await);
    assert_eq!(
        ok(index
            .dedup_xorb(&first.unwrap_or_default(), A, Visibility::PerTenant)
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

    let per_tenant = ok(index.chunk_tables(&asked, A, Visibility::PerTenant).await);
    assert_eq!(per_tenant, HashMap::from([(xa, ta.clone())]));

    let global = ok(index.chunk_tables(&asked, A, Visibility::Global).await);
    assert_eq!(global, HashMap::from([(xa, ta), (xb, tb)]));

    assert!(ok(index.chunk_tables(&[], A, Visibility::Global).await).is_empty());
}

#[tokio::test]
async fn chunk_tables_and_layouts_handle_more_xorbs_than_one_query_binds() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let mut asked = Vec::new();
    for i in 0..1200 {
        asked.push(put(&index, &format!("x{i}"), 1, A).await.0);
    }
    let tables = ok(index.chunk_tables(&asked, A, Visibility::PerTenant).await);
    assert_eq!(tables.len(), 1200);
    let layouts = ok(index.chunk_layouts(&asked, A, Visibility::PerTenant).await);
    assert_eq!(layouts.len(), 1200);
}

/// `table`'s lengths and offsets: the layout the index must answer for it.
fn layout_of(table: &ChunkTable) -> ChunkLayout {
    ChunkLayout {
        lengths: table
            .hashes
            .iter()
            .map(|(_, len)| u32::try_from(*len).unwrap_or(u32::MAX))
            .collect(),
        offsets: table.offsets.clone(),
    }
}

/// A table whose chunks differ in length, so a layout that mixes up
/// lengths and offsets or entries can't pass.
fn uneven_table(label: &str, n: u32) -> ChunkTable {
    let lengths: Vec<u64> = (0..n).map(|i| u64::from(1000 + i * 37)).collect();
    let mut offsets = vec![0u32];
    for i in 0..n {
        offsets.push(offsets[i as usize] + 500 + i * 11);
    }
    ChunkTable {
        hashes: lengths
            .iter()
            .enumerate()
            .map(|(i, len)| (h(&format!("{label}/chunk{i}")), *len))
            .collect(),
        offsets: offsets.into(),
    }
}

#[tokio::test]
async fn chunk_layouts_answer_as_chunk_tables_do_without_hashes() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xa, ta) = (h("xorb/a"), uneven_table("a", 7));
    let (xb, tb) = (h("xorb/b"), uneven_table("b", 3));
    ok(index.put_xorb(&xa, 9000, &ta, &[], A).await);
    ok(index.put_xorb(&xb, 9000, &tb, &[], B).await);
    // A second put keeps the first table, and so the first layout.
    ok(index.put_xorb(&xa, 9000, &tb, &[], A).await);
    let missing = h("xorb/missing");
    let asked = [xa, xb, missing, xa];

    let per_tenant = ok(index.chunk_layouts(&asked, A, Visibility::PerTenant).await);
    assert_eq!(per_tenant, HashMap::from([(xa, layout_of(&ta))]));
    let global = ok(index.chunk_layouts(&asked, A, Visibility::Global).await);
    assert_eq!(
        global,
        HashMap::from([(xa, layout_of(&ta)), (xb, layout_of(&tb))])
    );
    assert!(ok(index.chunk_layouts(&[], A, Visibility::Global).await).is_empty());
}

#[tokio::test]
async fn chunk_layouts_of_xorbs_stored_before_layouts_come_from_their_tables() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let index = open_at(&path).await;
    let (xorb, t) = (h("xorb/old"), uneven_table("old", 5));
    ok(index.put_xorb(&xorb, 9000, &t, &[], A).await);
    drop(index);
    // As a database written before migration 0003 holds it.
    let db = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    ok(sqlx::query("DELETE FROM xorb_layouts").execute(&db).await);
    db.close().await;

    let index = open_at(&path).await;
    assert_eq!(
        ok(index.chunk_layouts(&[xorb], A, Visibility::PerTenant).await),
        HashMap::from([(xorb, layout_of(&t))])
    );
}

#[tokio::test]
async fn registration_records_where_each_term_is_stored() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let index = open_at(&path).await;
    let (xa, ta) = (h("xorb/a"), uneven_table("a", 7));
    let (xb, tb) = (h("xorb/b"), uneven_table("b", 3));
    ok(index.put_xorb(&xa, 9000, &ta, &[], A).await);
    ok(index.put_xorb(&xb, 9000, &tb, &[], A).await);
    let terms = [(xa, 0, 2), (xb, 1, 3), (xa, 2, 7), (xa, 0, 1)];
    let record = FileRecord {
        hash: h("file/bytes"),
        size: 0,
        terms: terms
            .iter()
            .map(|&(xorb, chunk_start, chunk_end)| Term {
                xorb,
                chunk_start,
                chunk_end,
                unpacked_bytes: 1,
            })
            .collect(),
    };
    let new = NewFile {
        record: record.clone(),
        sha256: None,
        first_chunk: None,
    };
    ok(index
        .register_shard(&h("shard/bytes"), &[new], reg(A), Visibility::PerTenant)
        .await);
    let expected: Vec<(u32, u32)> = terms
        .iter()
        .map(|&(xorb, start, end)| {
            let offsets = if xorb == xa { &ta.offsets } else { &tb.offsets };
            (offsets[start as usize], offsets[end as usize])
        })
        .collect();
    assert_eq!(
        ok(index.file_with_bytes(&record.hash, A).await),
        Some((record.clone(), Some(expected)))
    );
    assert_eq!(ok(index.file_with_bytes(&record.hash, B).await), None);
    drop(index);

    // As a database written before migration 0004 holds it.
    let db = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    ok(sqlx::query("UPDATE file_scopes SET term_bytes = NULL")
        .execute(&db)
        .await);
    db.close().await;
    let index = open_at(&path).await;
    assert_eq!(
        ok(index.file_with_bytes(&record.hash, A).await),
        Some((record, None))
    );
}

#[tokio::test]
async fn a_term_past_its_xorbs_chunks_is_refused_at_registration() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (xorb, _) = put(&index, "x", 3, A).await;
    let mut f = file("past", xorb, 3);
    f.record.terms[0].chunk_end = 4;
    let refused = index
        .register_shard(
            &h("shard/past"),
            &[f.clone()],
            reg(A),
            Visibility::PerTenant,
        )
        .await;
    assert!(
        matches!(refused, Err(IndexError::InvalidInput(_))),
        "{refused:?}"
    );
    assert_eq!(ok(index.file(&f.record.hash, A).await), None);
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
                reg(FAULT_AFTER_FIRST_FILE_SCOPE),
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
    // The shard_registrations row rolled back too: registering the shard again in
    // the same scope is new. No files means the fault point isn't reached.
    assert!(ok(index
        .register_shard(
            &shard,
            &[],
            reg(FAULT_AFTER_FIRST_FILE_SCOPE),
            Visibility::Global
        )
        .await));
    assert!(!ok(index
        .register_shard(
            &shard,
            &[],
            reg(FAULT_AFTER_FIRST_FILE_SCOPE),
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
            reg(A),
            Visibility::PerTenant,
        )
        .await);
    let first = t.hashes[0].0;
    drop(index);

    let index: Arc<dyn Index> = Arc::new(open_at(&path).await);
    assert_eq!(
        ok(index.xorb_size(&xorb, A, Visibility::PerTenant).await),
        Some(1900)
    );
    assert_eq!(
        ok(index.chunk_tables(&[xorb], A, Visibility::PerTenant).await),
        HashMap::from([(xorb, t)])
    );
    assert_eq!(
        ok(index.file(&f.record.hash, A).await),
        Some(f.record.clone())
    );
    assert_eq!(
        ok(index.dedup_xorb(&first, A, Visibility::PerTenant).await),
        Some(xorb)
    );
    assert!(!ok(index
        .register_shard(&h("shard"), &[f], reg(A), Visibility::PerTenant)
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
        .register_shard(&h("shard"), &files, reg(A), Visibility::PerTenant)
        .await);

    for xorb in [x1, x2, x3, x4] {
        for vis in [Visibility::Global, Visibility::PerTenant] {
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
            reg(A),
            Visibility::Global,
        )
        .await);

    for vis in [Visibility::Global, Visibility::PerTenant] {
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
        .register_shard(&h("old"), &old, reg(A), Visibility::PerTenant)
        .await);
    ok(index
        .register_shard(&h("new"), &new, reg(A), Visibility::PerTenant)
        .await);
    // Registering the older shard again changes nothing, so it stays older.
    assert!(!ok(index
        .register_shard(&h("old"), &old, reg(A), Visibility::PerTenant)
        .await));

    for vis in [Visibility::Global, Visibility::PerTenant] {
        assert_eq!(
            ok(index.dedup_shard_xorbs(&x1, A, vis).await),
            Some(vec![x3, x1])
        );
        assert_eq!(
            ok(index.dedup_shard_xorbs(&x2, A, vis).await),
            Some(vec![x1, x2])
        );
    }

    // Under `PerTenant` only the caller's registrations count: B registers
    // the old shard later, which makes it the newest for B and, under
    // `Global`, for everyone, but not for A under `PerTenant`.
    for x in [x1, x2] {
        ok(index
            .put_xorb(&x, 1300, &table(&format!("{x}"), 2), &[], B)
            .await);
    }
    ok(index
        .register_shard(&h("old"), &old, reg(B), Visibility::PerTenant)
        .await);
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, B, Visibility::PerTenant).await),
        Some(vec![x1, x2])
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::Global).await),
        Some(vec![x1, x2])
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::PerTenant).await),
        Some(vec![x3, x1])
    );
}

#[tokio::test]
async fn dedup_shard_xorbs_respects_tenant_and_visibility() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x1, _) = put(&index, "x1", 2, A).await;
    let (x2, _) = put(&index, "x2", 2, B).await;

    // Another tenant's shard is never offered under `PerTenant`, even when
    // the caller holds the xorb itself.
    ok(index
        .register_shard(
            &h("a-only"),
            &[file_over("a", &[(x1, 2)])],
            reg(A),
            Visibility::PerTenant,
        )
        .await);
    ok(index.put_xorb(&x1, 1300, &table("x1", 2), &[], B).await);
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, B, Visibility::PerTenant).await),
        None
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, B, Visibility::Global).await),
        Some(vec![x1])
    );

    // A shard registered under `Global` may mix xorbs of several tenants.
    // Registering it makes its tenant an owner of all of them (ruling
    // M1-R39), so read under `PerTenant` it lists them all to that tenant,
    // and still none to a tenant without a registration.
    ok(index
        .register_shard(
            &h("mixed"),
            &[file_over("m", &[(x2, 2), (x1, 2)])],
            reg(A),
            Visibility::Global,
        )
        .await);
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::PerTenant).await),
        Some(vec![x2, x1])
    );
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x1, A, Visibility::Global).await),
        Some(vec![x2, x1])
    );
    assert_eq!(
        ok(index
            .dedup_shard_xorbs(&x1, "c", Visibility::PerTenant)
            .await),
        None
    );
}

/// Registration numbers count up across reopens and processes: the index
/// hands each out once, and a registration's bucket record carries it.
#[tokio::test]
async fn registration_seq_counts_up_across_reopens() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let index = open_at(&path).await;
    let first = ok(index.next_registration_seq().await);
    assert_eq!(ok(index.next_registration_seq().await), first + 1);
    drop(index);
    let other = open_at(&path).await;
    assert_eq!(ok(other.next_registration_seq().await), first + 2);
}

/// The newest registration a dedup answer looks up is found through the
/// index on `registered_seq`, not by visiting every registration.
#[tokio::test]
async fn newest_registration_is_an_index_lookup() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    drop(open_at(&path).await);
    let pool = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
    let plan: Vec<(i64, i64, i64, String)> = ok(sqlx::query_as(
        "EXPLAIN QUERY PLAN SELECT shard FROM shard_registrations ORDER BY registered_seq DESC \
         LIMIT 1",
    )
    .fetch_all(&pool)
    .await);
    let details: Vec<&str> = plan.iter().map(|row| row.3.as_str()).collect();
    assert!(
        details
            .iter()
            .any(|d| d.contains("shard_registrations_registered_seq")),
        "plan sorts every shard registration: {details:?}"
    );
}

/// Ruling M1-R39 (M1 final review I2): a file registered under `Global`
/// over another tenant's xorb stays readable to its own tenant after the
/// operator switches to `PerTenant`, and dedup still offers its upload.
#[tokio::test]
async fn global_registration_stays_readable_under_per_tenant() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x, _) = put(&index, "x", 2, A).await;
    ok(index
        .register_shard(
            &h("shard-b"),
            &[file_over("b", &[(x, 2)])],
            reg(B),
            Visibility::Global,
        )
        .await);

    let layouts = ok(index.chunk_layouts(&[x], B, Visibility::PerTenant).await);
    assert_eq!(layouts.len(), 1, "B's file needs its xorb's layout");
    assert_eq!(
        ok(index.dedup_shard_xorbs(&x, B, Visibility::PerTenant).await),
        Some(vec![x])
    );
    assert!(
        ok(index.xorb_size(&x, B, Visibility::PerTenant).await).is_some(),
        "B owns the xorb its file references"
    );
    assert!(
        ok(index.chunk_layouts(&[x], "c", Visibility::PerTenant).await).is_empty(),
        "a third tenant is still isolated"
    );
}

/// Scopes are the unit of access, tenants of isolation (ruling M1-R38):
/// two scopes of one tenant share xorbs under `PerTenant`, while files stay
/// per scope; another tenant sees neither.
#[tokio::test]
async fn scopes_of_one_tenant_share_xorbs_under_per_tenant() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x, _) = put(&index, "x", 2, "org").await;
    let f = file_over("f", &[(x, 2)]);
    let in_org = |scope| Registrant {
        scope,
        tenant: "org",
        seq: reg(scope).seq,
    };
    ok(index
        .register_shard(
            &h("base"),
            std::slice::from_ref(&f),
            in_org("org/base"),
            Visibility::PerTenant,
        )
        .await);
    ok(index
        .register_shard(
            &h("fine-tune"),
            std::slice::from_ref(&f),
            in_org("org/fine-tune"),
            Visibility::PerTenant,
        )
        .await);
    assert!(ok(index.file(&f.record.hash, "org/fine-tune").await).is_some());
    assert_eq!(
        ok(index
            .dedup_xorb(&h("x/chunk0"), "org", Visibility::PerTenant)
            .await),
        Some(x)
    );
    assert!(ok(index.file(&f.record.hash, "other/repo").await).is_none());
    assert_eq!(
        ok(index
            .dedup_xorb(&h("x/chunk0"), "other", Visibility::PerTenant)
            .await),
        None
    );
    let err = index
        .register_shard(
            &h("other"),
            std::slice::from_ref(&f),
            reg("other/repo"),
            Visibility::PerTenant,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, IndexError::UnknownXorb { xorb } if xorb == x),
        "{err}"
    );
}

/// Of two registrations of one file to one scope, the lower-numbered one's
/// reconstruction is kept, whichever commits first: the row a rebuild from
/// the bucket's records picks (ruling M1-R37).
#[tokio::test]
async fn lowest_numbered_registration_of_a_file_wins() {
    let dir = ok(TempDir::new());
    let index = open(&dir).await;
    let (x1, _) = put(&index, "x1", 2, A).await;
    let (x2, _) = put(&index, "x2", 2, A).await;
    let mut over_x1 = file_over("f", &[(x1, 2)]);
    over_x1.sha256 = Some([1; 32]);
    let mut over_x2 = file_over("f", &[(x2, 2)]);
    over_x2.sha256 = Some([2; 32]);
    let at = |seq| Registrant {
        scope: A,
        tenant: A,
        seq,
    };
    ok(index
        .register_shard(
            &h("later"),
            &[over_x2.clone()],
            at(20),
            Visibility::PerTenant,
        )
        .await);
    ok(index
        .register_shard(
            &h("earlier"),
            &[over_x1.clone()],
            at(10),
            Visibility::PerTenant,
        )
        .await);
    assert_eq!(
        ok(index.file(&over_x1.record.hash, A).await),
        Some(over_x1.record.clone())
    );
    ok(index
        .register_shard(&h("latest"), &[over_x2], at(30), Visibility::PerTenant)
        .await);
    assert_eq!(
        ok(index.file(&over_x1.record.hash, A).await),
        Some(over_x1.record)
    );
}

/// Migration 0005 carries a database written before tenants existed: each
/// scope becomes its own tenant, `scope:` and its name (ruling M1-R43),
/// owning the xorbs it uploaded and those its registered shards reference (ruling M1-R39), and registration numbers
/// continue past the highest one recorded.
#[tokio::test]
async fn migration_0005_makes_each_scope_its_own_tenant() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = ok(sqlx::SqlitePool::connect(&url).await);
    ok(sqlx::migrate!("./migrations").run_to(4, &db).await);
    let (x1, x2, shard, f) = (h("x1"), h("x2"), h("shard"), h("file"));
    let mut table = h("chunk").as_bytes().to_vec();
    table.extend_from_slice(&1000u32.to_le_bytes());
    table.extend_from_slice(&0u32.to_le_bytes());
    table.extend_from_slice(&600u32.to_le_bytes());
    for (seq, xorb) in [(1, x1), (2, x2)] {
        ok(sqlx::query(
            "INSERT INTO xorbs (hash, size, chunk_count, chunk_table, ingested_seq) \
             VALUES (?, 700, 1, ?, ?)",
        )
        .bind(xorb.as_bytes())
        .bind(&table)
        .bind(seq)
        .execute(&db)
        .await);
    }
    ok(
        sqlx::query("INSERT INTO xorb_scopes (xorb, scope) VALUES (?, 'uploader')")
            .bind(x1.as_bytes())
            .execute(&db)
            .await,
    );
    ok(
        sqlx::query("INSERT INTO shard_scopes (shard, scope, registered_seq) VALUES (?, ?, 7)")
            .bind(shard.as_bytes())
            .bind(A)
            .execute(&db)
            .await,
    );
    for (pos, xorb) in [(0, x1), (1, x2)] {
        ok(
            sqlx::query("INSERT INTO shard_xorbs (shard, pos, xorb) VALUES (?, ?, ?)")
                .bind(shard.as_bytes())
                .bind(pos)
                .bind(xorb.as_bytes())
                .execute(&db)
                .await,
        );
    }
    ok(sqlx::query(
        "INSERT INTO file_scopes (file, scope, size, terms, sha256, shard) \
         VALUES (?, ?, 0, x'', NULL, ?)",
    )
    .bind(f.as_bytes())
    .bind(A)
    .bind(shard.as_bytes())
    .execute(&db)
    .await);
    db.close().await;

    let index = open_at(&path).await;
    // A credential for scope `s` naming no tenant is in tenant `scope:s`
    // (ruling M1-R43).
    let tenant_a = format!("scope:{A}");
    for (xorb, tenant) in [(x1, "scope:uploader"), (x1, &tenant_a), (x2, &tenant_a)] {
        assert!(
            ok(index.xorb_size(&xorb, tenant, Visibility::PerTenant).await).is_some(),
            "{tenant} owns {xorb}"
        );
    }
    assert!(
        ok(index
            .xorb_size(&x2, "scope:uploader", Visibility::PerTenant)
            .await)
        .is_none()
    );
    assert_eq!(
        ok(index
            .dedup_shard_xorbs(&x1, &tenant_a, Visibility::PerTenant)
            .await),
        Some(vec![x1, x2])
    );
    assert!(ok(index.xorb_size(&x1, A, Visibility::PerTenant).await).is_none());
    assert_eq!(ok(index.next_registration_seq().await), 8);
    let db = ok(sqlx::SqlitePool::connect(&url).await);
    let seq: i64 = ok(sqlx::query_scalar("SELECT registered_seq FROM file_scopes")
        .fetch_one(&db)
        .await);
    assert_eq!(seq, 7);
    let tenant: String = ok(sqlx::query_scalar("SELECT tenant FROM shard_registrations")
        .fetch_one(&db)
        .await);
    assert_eq!(tenant, tenant_a);
}
