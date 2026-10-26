//! Files stored as the CAS stores them, for the gateway and memory tests:
//! xorbs of uncompressed chunks in the bucket, their layouts and the file's
//! terms in the index, registered to a repository's scope.

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::{Method, Request};
use openxet_hub::test_support::{BEARER, Reply, TestHub};
use openxet_index::{ChunkTable, FileRecord, NewFile, Registrant, Term, Visibility};
use openxet_protocol::{chunk_hash, file_hash, hash_to_hex, xorb_with_footer};
use openxet_storage::Key;
use sha2::Digest as _;
use tower::ServiceExt as _;

/// A file stored as the CAS stores one: its xorbs in the bucket and their
/// chunk layouts and the file's terms in the index, registered to
/// `repo`'s scope with its SHA-256.
pub struct Stored {
    pub hash: String,
    pub sha256: String,
    pub content: Vec<u8>,
    /// Where each term's chunks start in the file, and where it ends.
    pub boundaries: Vec<u64>,
}

/// One uncompressed chunk holding `data`, framed per the spec's chunk
/// header (<https://huggingface.co/docs/xet/xorb#chunk-header-structure>).
pub fn chunk(data: &[u8]) -> Vec<u8> {
    let len = u32::try_from(data.len()).unwrap().to_le_bytes();
    let mut out = vec![0, len[0], len[1], len[2], 0, len[0], len[1], len[2]];
    out.extend_from_slice(data);
    out
}

/// Stores `xorbs` (each a list of chunk contents) and registers to `repo`
/// the file whose terms are `terms`, `(xorb, first chunk, end chunk)`.
pub async fn store_file(
    hub: &TestHub,
    repo: &str,
    xorbs: &[Vec<Vec<u8>>],
    terms: &[(usize, u32, u32)],
) -> Stored {
    let how = Storing {
        in_bucket: true,
        named: true,
    };
    index_file(hub, repo, xorbs, terms, how).await
}

/// How [`index_file`] stores a file.
#[derive(Debug, Clone, Copy)]
pub struct Storing {
    /// Whether its xorbs reach the bucket, or only the index.
    pub in_bucket: bool,
    /// Whether its shard names its SHA-256.
    pub named: bool,
}

/// [`store_file`] as `how` says.
pub async fn index_file(
    hub: &TestHub,
    repo: &str,
    xorbs: &[Vec<Vec<u8>>],
    terms: &[(usize, u32, u32)],
    how: Storing,
) -> Stored {
    let scope = format!("repo:{repo}");
    let index = &hub.state.index;
    let mut hashes = Vec::new();
    for chunks in xorbs {
        let body: Vec<u8> = chunks.iter().flat_map(|c| chunk(c)).collect();
        let stored = xorb_with_footer(&body).unwrap();
        let size = stored.bytes.len() as u64;
        let table = ChunkTable {
            hashes: stored.chunks.hashes.clone().into(),
            offsets: stored.chunk_offsets.clone().into(),
        };
        if how.in_bucket {
            hub.state
                .bucket
                .put(&Key::Xorb(stored.hash), Bytes::from(stored.bytes))
                .await
                .unwrap();
        }
        index
            .put_xorb(&stored.hash, size, &table, &[], "alice")
            .await
            .unwrap();
        hashes.push(stored.hash);
    }
    let (mut content, mut boundaries, mut chunk_list) = (Vec::new(), vec![0], Vec::new());
    let mut record_terms = Vec::new();
    for &(x, start, end) in terms {
        let mut bytes = 0u32;
        for c in &xorbs[x][start as usize..end as usize] {
            content.extend_from_slice(c);
            chunk_list.push((chunk_hash(c), c.len() as u64));
            bytes += u32::try_from(c.len()).unwrap();
        }
        boundaries.push(content.len() as u64);
        record_terms.push(Term {
            xorb: hashes[x],
            chunk_start: start,
            chunk_end: end,
            unpacked_bytes: bytes,
        });
    }
    let hash = file_hash(&chunk_list);
    let sha256: [u8; 32] = sha2::Sha256::digest(&content).into();
    let new = NewFile {
        record: FileRecord {
            hash,
            size: content.len() as u64,
            terms: record_terms,
        },
        sha256: how.named.then_some(sha256),
        first_chunk: Some(chunk_list[0].0),
    };
    let seq = index.next_registration_seq().await.unwrap();
    let by = Registrant {
        scope: &scope,
        tenant: "alice",
        seq,
    };
    index
        .register_shard(&chunk_hash(&content), &[new], by, Visibility::Global)
        .await
        .unwrap();
    Stored {
        hash: hash_to_hex(&hash),
        sha256: hex(&sha256),
        content,
        boundaries,
    }
}

/// `n` bytes of a pattern that differs per `seed`.
pub fn data(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| {
            seed.wrapping_mul(31)
                .wrapping_add(u8::try_from(i % 251).unwrap())
        })
        .collect()
}

/// A file over three xorbs whose terms revisit the first xorb, so its
/// plan has fetches of one xorb apart: chunks of 1,000 to 7,000 bytes.
pub async fn three_xorb_file(hub: &TestHub, repo: &str) -> Stored {
    let xorbs: Vec<Vec<Vec<u8>>> = (0..3u8)
        .map(|x| {
            (0..5u8)
                .map(|c| data(x * 10 + c, 1000 + 1500 * usize::from((x + c) % 5)))
                .collect()
        })
        .collect();
    store_file(
        hub,
        repo,
        &xorbs,
        &[
            (0, 0, 2),
            (1, 0, 3),
            (0, 1, 4),
            (2, 0, 2),
            (1, 3, 5),
            (2, 2, 5),
        ],
    )
    .await
}

/// Sends `method path` through `router` with `headers` (name, value).
pub async fn request(
    router: &Router,
    method: Method,
    path: &str,
    headers: &[(&str, String)],
) -> Reply {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, value);
    }
    let response = router
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    Reply {
        status,
        headers,
        body,
    }
}

pub fn bearer(token: &str) -> (&'static str, String) {
    ("authorization", format!("{BEARER} {token}"))
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}
