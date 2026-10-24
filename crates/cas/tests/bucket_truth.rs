//! The bucket is the source of truth (non-negotiable 2, ruling M1-R37):
//! after a realistic workload, the index's scope tables are exactly what
//! the bucket's records, shards and xorbs rebuild.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::shard::hf_xet_shard;
use common::xorb::{hf_xet_xorb, stored, xorb_of, xorb_path};
use common::{Reply, SCOPE_A, SCOPE_B, TestServer, bearer, config};
use openxet_auth::{Mode, default_tenant};
use openxet_index::{SqliteIndex, Visibility, rebuild_scope_tables};
use openxet_protocol::{
    MerkleHash, Segment, ShardFileEntry, UploadShard, file_hash, range_hash, stored_shard,
};
use reqwest::Method;
use reqwest::header::AUTHORIZATION;

/// A xorb of `n` distinct uncompressed chunks labelled `label`.
fn xorb_body(label: &str, n: usize) -> Vec<u8> {
    let data: Vec<Vec<u8>> = (0..n)
        .map(|i| format!("{label}/chunk{i}/{}", "x".repeat(64)).into_bytes())
        .collect();
    let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
    xorb_of(&refs)
}

/// The chunk tables of the xorbs a workload uploaded, by hash.
type Tables = HashMap<MerkleHash, Arc<[(MerkleHash, u64)]>>;

/// A file over chunk ranges `(xorb, start, end)` of `tables`' xorbs, as a
/// client's shard lists it.
fn file(tables: &Tables, ranges: &[(MerkleHash, u32, u32)], sha256: Option<u8>) -> ShardFileEntry {
    let mut chunks = Vec::new();
    let segments = ranges
        .iter()
        .map(|&(xorb, start, end)| {
            let range = &tables[&xorb][start as usize..end as usize];
            chunks.extend_from_slice(range);
            let hashes: Vec<MerkleHash> = range.iter().map(|(h, _)| *h).collect();
            Segment {
                xorb,
                chunk_start: start,
                chunk_end: end,
                unpacked_bytes: u32::try_from(range.iter().map(|(_, l)| l).sum::<u64>()).unwrap(),
                verification: Some(range_hash(&hashes)),
            }
        })
        .collect();
    ShardFileEntry {
        file_hash: file_hash(&chunks),
        segments,
        sha256: sha256.map(|b| [b; 32]),
    }
}

/// An upload shard of `files`, in the form clients send.
fn shard(tables: &Tables, files: Vec<ShardFileEntry>) -> Vec<u8> {
    stored_shard(
        &UploadShard {
            files,
            xorbs: Vec::new(),
        },
        tables,
    )
    .unwrap()
}

async fn post_xorb(server: &TestServer, body: Vec<u8>, token: &str) -> MerkleHash {
    let hash = stored(&body).hash;
    let reply = Reply::of(
        server
            .request(Method::POST, &xorb_path(&hash))
            .header(AUTHORIZATION, bearer(token))
            .body(body),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    hash
}

/// Uploads xorb `label` of `n` chunks with `token`, recording its table.
async fn put(
    tables: &mut Tables,
    server: &TestServer,
    label: &str,
    n: usize,
    token: &str,
) -> MerkleHash {
    let body = xorb_body(label, n);
    let chunks = stored(&body).chunks.hashes;
    let hash = post_xorb(server, body, token).await;
    tables.insert(hash, chunks.into());
    hash
}

async fn post_shard(server: &TestServer, body: Vec<u8>, token: &str) -> u64 {
    let reply = Reply::of(
        server
            .request(Method::POST, "/v1/shards")
            .header(AUTHORIZATION, bearer(token))
            .body(body),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()["result"].as_u64().unwrap()
}

/// Several scopes and tenants, both visibility modes, files registered
/// twice to one scope with different terms, shards uploaded again, and a
/// tenant registering files over another tenant's xorbs under `global`:
/// the index's file registrations (terms, SHA-256, term byte ranges and
/// sequence numbers included), shard registrations, xorb owners and shard
/// term orders all rebuild from the bucket alone.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one workload across a restart, then one comparison"
)]
async fn index_scope_tables_rebuild_from_the_bucket() {
    let server = TestServer::start(config(Visibility::Global)).await;
    let mut tables = Tables::new();

    // Under `global`: scope A uploads two xorbs and registers two files.
    let x1 = put(&mut tables, &server, "x1", 3, &server.write_token).await;
    let x2 = put(&mut tables, &server, "x2", 4, &server.write_token).await;
    let f1 = file(&tables, &[(x1, 0, 3)], Some(1));
    let f2 = file(&tables, &[(x2, 0, 2), (x1, 1, 2)], None);
    let s1 = shard(&tables, vec![f2.clone(), f1.clone()]);
    assert_eq!(
        post_shard(&server, s1.clone(), &server.write_token).await,
        1
    );
    assert_eq!(post_shard(&server, s1, &server.write_token).await, 0);
    // Scope B, its own tenant, registers a file over its xorb and A's.
    let x3 = put(&mut tables, &server, "x3", 2, &server.other_scope_token).await;
    let f3 = file(&tables, &[(x3, 0, 2), (x1, 0, 1)], Some(3));
    let s2 = shard(&tables, vec![f3.clone(), f1.clone()]);
    assert_eq!(post_shard(&server, s2, &server.other_scope_token).await, 1);
    // A platform's tenant uploads the `hf_xet` capture to one of its scopes.
    let base = server.token("org/base", Some("org"), Mode::Write);
    post_xorb(&server, hf_xet_xorb(), &base).await;
    assert_eq!(post_shard(&server, hf_xet_shard(), &base).await, 1);

    // Under `per-tenant`, after a restart on the same data.
    let server = server.restart(config(Visibility::PerTenant)).await;
    let tune = server.token("org/fine-tune", Some("org"), Mode::Write);
    assert_eq!(post_shard(&server, hf_xet_shard(), &tune).await, 1);
    // A registers f2 again with other terms over the same chunks, and a new
    // file: the first registration's terms stay.
    let f2_again = file(&tables, &[(x2, 0, 1), (x2, 1, 2), (x1, 1, 2)], None);
    assert_eq!(f2_again.file_hash, f2.file_hash);
    let f4 = file(&tables, &[(x2, 2, 4)], Some(4));
    let s3 = shard(&tables, vec![f2_again, f4]);
    assert_eq!(post_shard(&server, s3, &server.write_token).await, 1);
    // B, which owns x1 since its registration, reuses it.
    let f5 = file(&tables, &[(x1, 0, 2)], None);
    assert_eq!(
        post_shard(&server, shard(&tables, vec![f5]), &server.other_scope_token).await,
        1
    );
    // A tenant with no claim, so its own tenant, uploads x3 again.
    let solo = server.token("solo/repo", None, Mode::Write);
    put(&mut tables, &server, "x3", 2, &solo).await;
    let f6 = file(&tables, &[(x3, 1, 2)], Some(6));
    assert_eq!(
        post_shard(&server, shard(&tables, vec![f6]), &solo).await,
        1
    );

    let live = SqliteIndex::open(&server.index_path())
        .await
        .unwrap()
        .scope_tables()
        .await
        .unwrap();
    let bucket = Arc::clone(&server.bucket);
    let rebuilt = server
        .on_server(async move { rebuild_scope_tables(bucket.as_ref()).await.unwrap() })
        .await;
    assert_eq!(rebuilt, live);

    // The workload reached what it meant to.
    assert_eq!(live.registrations.len(), 7);
    for scope in [SCOPE_A, SCOPE_B, "org/base", "org/fine-tune", "solo/repo"] {
        assert!(
            live.files.keys().any(|(_, s)| s == scope),
            "nothing registered to {scope}"
        );
    }
    let f2_row = &live.files[&(f2.file_hash, SCOPE_A.to_owned())];
    assert_eq!(f2_row.terms.len(), 2, "the first registration's terms stay");
    assert!(live.owners.contains(&(x1, default_tenant(SCOPE_B))));
    assert!(live.owners.contains(&(x3, default_tenant("solo/repo"))));
    assert!(live.files.values().any(|row| row.sha256.is_some()));
}
