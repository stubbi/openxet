//! What only the SQLite index shows: its rows, its query plans, its errors
//! and its migrations of older databases. The `Index` contract itself is
//! the conformance suite's (`sqlite_conformance.rs`).

use std::collections::HashMap;
use std::path::Path;

use std::sync::atomic::{AtomicU64, Ordering};

use openxet_index::{
    ChunkLayout, ChunkTable, FileRecord, Index, IndexError, NewFile, Registrant, SqliteIndex, Term,
    Visibility,
};
use openxet_protocol::{MerkleHash, chunk_hash};
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

/// A file registered before migration 0004 has no recorded term bytes:
/// `file_with_bytes` answers `None` for them, and callers read the bytes
/// from the xorbs instead.
#[tokio::test]
async fn files_registered_before_term_bytes_have_none() {
    let dir = ok(TempDir::new());
    let path = dir.path().join("index.sqlite");
    let index = open_at(&path).await;
    let (xorb, t) = (h("xorb/a"), uneven_table("a", 7));
    ok(index.put_xorb(&xorb, 9000, &t, &[], A).await);
    let record = FileRecord {
        hash: h("file/bytes"),
        size: 0,
        terms: vec![Term {
            xorb,
            chunk_start: 0,
            chunk_end: 2,
            unpacked_bytes: 1,
        }],
    };
    let new = NewFile {
        record: record.clone(),
        sha256: None,
        first_chunk: None,
    };
    ok(index
        .register_shard(&h("shard/bytes"), &[new], reg(A), Visibility::PerTenant)
        .await);
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

/// Sequence numbers can repeat once an index is lost or restored from an
/// older backup. Of two registrations of one shard with one number, the
/// lower tenant's row is kept, in either commit order, as a rebuild from
/// the bucket keeps it (F-A review m-3). The tie on the shard is the
/// conformance suite's `ties_on_the_number_break_on_shard`.
#[tokio::test]
async fn ties_on_the_number_and_shard_break_on_tenant() {
    let (s1, s2) = (h("shard-1"), h("shard-2"));
    let low = if s1.as_bytes() < s2.as_bytes() {
        s1
    } else {
        s2
    };
    for order in [["t2", "t1"], ["t1", "t2"]] {
        let dir = ok(TempDir::new());
        let path = dir.path().join("index.sqlite");
        let index = open_at(&path).await;
        let (x, _) = put(&index, "x", 2, "t1").await;
        ok(index.put_xorb(&x, 1300, &table("x", 2), &[], "t2").await);
        for tenant in order {
            ok(index
                .register_shard(
                    &low,
                    &[file_over("g", &[(x, 2)])],
                    Registrant {
                        scope: A,
                        tenant,
                        seq: 5,
                    },
                    Visibility::Global,
                )
                .await);
        }
        let db = ok(sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display())).await);
        let tenant: String = ok(sqlx::query_scalar(
            "SELECT registered_tenant FROM file_scopes WHERE scope = ?",
        )
        .bind(A)
        .fetch_one(&db)
        .await);
        assert_eq!(
            tenant, "t1",
            "the lower tenant wins a tie on number and shard"
        );
        db.close().await;
    }
}

/// Migration 0005 carries a database written before tenants existed: each
/// scope becomes its own tenant, `scope:` and its name (ruling M1-R43),
/// owning the xorbs it uploaded and those its registered shards reference
/// (ruling M1-R39), and registration numbers continue past the highest one
/// recorded.
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
