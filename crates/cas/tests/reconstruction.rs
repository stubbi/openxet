//! Reconstruction (v2, v1 and batch), `HEAD /v1/files` and the signed
//! transfer route (S10 §1, §5; ADR 0002; design §5.5, §5.9 req 1 and 2).
//!
//! Downloads here follow `hf_xet` 1.7.0: every data URL is fetched with no
//! credential and exactly its descriptor's `Range`, and its body is decoded
//! with upstream's chunk reader (S10 "Data URLs").

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::shard::{Event, hf_xet_files, hf_xet_shard, lines, terminal};
use common::xorb::{HF_XET_XORB, hf_xet_xorb, stored, xorb_of, xorb_path};
use common::{
    Access, BATCH_GET_PATH, BATCH_POST_PATH, DEDUP_MERKLEDB_PATH, DEDUP_PATH, FILE_PATH,
    RECONSTRUCTION_V1_PATH, RECONSTRUCTION_V2_PATH, ROUTES, Reply, Route, SCOPE_A, TRANSFER_PATH,
    TablelessIndex, TestServer, XORB_PATH, bearer, config,
};
use openxet_auth::UrlSigner;
use openxet_cas::{Clock as _, MAX_BATCH_FILES};
use openxet_index::{FileRecord, NewFile, Term, Visibility};
use openxet_protocol::{
    MerkleHash, chunk_hash, decode_fetched_chunks, file_hash, hash_from_hex, hash_to_hex,
    read_upload_shard,
};
use reqwest::Method;
use reqwest::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use serde_json::{Value, json};
use url::Url;

/// The first block `hf_xet` 1.7.0 asks for (S10 §1).
const FIRST_BLOCK: &str = "bytes=0-255999999";

/// The test server's transfer key (`common::TestServer`), so tests can mint
/// URLs the signer never would.
const TRANSFER_KEY: [u8; 32] = [7; 32];

/// The `hf_xet` 1.7.0 capture's files, in shard order (the empty file,
/// `a.bin`, `b.bin`), with their bytes rebuilt from the capture's xorb
/// chunks and the shard's terms: the capture holds no other copy.
fn capture_files() -> Vec<(MerkleHash, Vec<u8>)> {
    let (data, offsets) = decode_fetched_chunks(&hf_xet_xorb()).unwrap();
    let xorb = hash_from_hex(HF_XET_XORB).unwrap();
    read_upload_shard(&hf_xet_shard()[..])
        .unwrap()
        .files
        .iter()
        .map(|file| {
            let mut bytes = Vec::new();
            for seg in &file.segments {
                assert_eq!(seg.xorb, xorb);
                let (a, b) = (seg.chunk_start as usize, seg.chunk_end as usize);
                bytes.extend_from_slice(&data[offsets[a] as usize..offsets[b] as usize]);
            }
            (file.file_hash, bytes)
        })
        .collect()
}

/// Uploads the capture's xorb and shard with `token`.
async fn upload_capture(server: &TestServer, token: &str) {
    let xorb = hash_from_hex(HF_XET_XORB).unwrap();
    let reply = Reply::of(
        server
            .request(Method::POST, &xorb_path(&xorb))
            .header(AUTHORIZATION, bearer(token))
            .body(hf_xet_xorb()),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let reply = Reply::of(
        server
            .request(Method::POST, "/v1/shards")
            .header(AUTHORIZATION, bearer(token))
            .body(hf_xet_shard()),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

/// A file over one uploaded xorb of uncompressed chunks.
struct Synthetic {
    hash: MerkleHash,
    bytes: Vec<u8>,
}

/// Uploads a xorb of `chunks` with `token`, then registers to `scope`, in
/// the index directly, a file made of `terms` (chunk ranges of that xorb).
async fn synthetic_file(
    server: &TestServer,
    token: &str,
    scope: &'static str,
    vis: Visibility,
    chunks: &[Vec<u8>],
    terms: &[(u32, u32)],
) -> Synthetic {
    let refs: Vec<&[u8]> = chunks.iter().map(Vec::as_slice).collect();
    let body = xorb_of(&refs);
    let xorb = stored(&body).hash;
    let reply = Reply::of(
        server
            .request(Method::POST, &xorb_path(&xorb))
            .header(AUTHORIZATION, bearer(token))
            .body(body),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);

    let mut bytes = Vec::new();
    let mut hashes = Vec::new();
    let mut record_terms = Vec::new();
    for &(start, end) in terms {
        let mut unpacked = 0;
        for chunk in &chunks[start as usize..end as usize] {
            bytes.extend_from_slice(chunk);
            hashes.push((chunk_hash(chunk), chunk.len() as u64));
            unpacked += u32::try_from(chunk.len()).unwrap();
        }
        record_terms.push(Term {
            xorb,
            chunk_start: start,
            chunk_end: end,
            unpacked_bytes: unpacked,
        });
    }
    let hash = file_hash(&hashes);
    let file = NewFile {
        record: FileRecord {
            hash,
            size: bytes.len() as u64,
            terms: record_terms,
        },
        sha256: None,
        first_chunk: hashes.first().map(|(h, _)| *h),
    };
    let shard = chunk_hash(hash.as_bytes());
    let index = Arc::clone(&server.index);
    server
        .on_server(async move {
            index
                .register_shard(&shard, &[file], scope, vis)
                .await
                .unwrap()
        })
        .await;
    Synthetic { hash, bytes }
}

/// Five uncompressed chunks of 7, 11, 13, 17 and 19 distinct bytes.
fn five_chunks() -> Vec<Vec<u8>> {
    let mut next = 0u8;
    [7, 11, 13, 17, 19]
        .iter()
        .map(|&len| {
            (0..len)
                .map(|_| {
                    next = next.wrapping_add(1);
                    next
                })
                .collect()
        })
        .collect()
}

/// `GET {version}/reconstructions/{file}` with `range` and `token`.
async fn reconstruct(
    server: &TestServer,
    version: &str,
    file: &MerkleHash,
    range: Option<&str>,
    token: &str,
) -> Reply {
    let mut request = server
        .request(
            Method::GET,
            &format!("/{version}/reconstructions/{}", hash_to_hex(file)),
        )
        .header(AUTHORIZATION, bearer(token));
    if let Some(range) = range {
        request = request.header(RANGE, range);
    }
    Reply::of(request).await
}

/// A transfer URL's answer: status, headers and the raw body.
struct Fetched {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

/// `GET url` with `range` as the `Range` header, and no credential.
async fn fetch(server: &TestServer, url: &str, range: Option<String>) -> Fetched {
    let mut request = server.http.get(url);
    if let Some(range) = range {
        request = request.header(RANGE, range);
    }
    let response = request.send().await.unwrap();
    Fetched {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.bytes().await.unwrap().to_vec(),
    }
}

fn range_of(value: &Value) -> (u64, u64) {
    (
        value["start"].as_u64().unwrap(),
        value["end"].as_u64().unwrap(),
    )
}

/// Downloads a v2 answer as `hf_xet` does and returns the reconstructed
/// bytes from `offset_into_first_range` on. Asserts S10's invariants on the
/// way: one descriptor per fetch entry (ADR 0002), every term inside one
/// descriptor of its xorb, every `unpacked_length` the decoded length, and a
/// `206` with exactly the bytes for every URL.
async fn download_v2(server: &TestServer, answer: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    for term in answer["terms"].as_array().unwrap() {
        let (a, b) = range_of(&term["range"]);
        let entries = answer["xorbs"][term["hash"].as_str().unwrap()]
            .as_array()
            .unwrap();
        assert!(
            entries
                .iter()
                .all(|e| e["ranges"].as_array().unwrap().len() == 1)
        );
        let entry = entries
            .iter()
            .find(|e| {
                let (s, e) = range_of(&e["ranges"][0]["chunks"]);
                s <= a && b <= e
            })
            .expect("every term lies inside one descriptor of its xorb");
        let descriptor = &entry["ranges"][0];
        let (first_chunk, _) = range_of(&descriptor["chunks"]);
        let (s, e) = range_of(&descriptor["bytes"]);
        let got = fetch(
            server,
            entry["url"].as_str().unwrap(),
            Some(format!("bytes={s}-{e}")),
        )
        .await;
        assert_eq!(got.status, 206);
        assert_eq!(got.body.len() as u64, e - s + 1);
        let (data, offsets) = decode_fetched_chunks(&got.body).unwrap();
        let lo = offsets[usize::try_from(a - first_chunk).unwrap()] as usize;
        let hi = offsets[usize::try_from(b - first_chunk).unwrap()] as usize;
        assert_eq!(
            (hi - lo) as u64,
            term["unpacked_length"].as_u64().unwrap(),
            "unpacked_length is the term's decoded length"
        );
        out.extend_from_slice(&data[lo..hi]);
    }
    let skip = usize::try_from(answer["offset_into_first_range"].as_u64().unwrap()).unwrap();
    out.drain(..skip);
    out
}

/// Reconstructs `file` over v2 with `range` and downloads it, keeping the
/// `len` bytes asked for, as the client trims.
async fn download(
    server: &TestServer,
    file: &MerkleHash,
    range: Option<&str>,
    len: usize,
) -> Vec<u8> {
    let reply = reconstruct(server, "v2", file, range, &server.read_token).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.headers[CONTENT_TYPE], "application/json");
    assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
    let mut bytes = download_v2(server, &reply.json()).await;
    assert!(bytes.len() >= len, "the terms stop short of the range");
    bytes.truncate(len);
    bytes
}

/// Every data URL in a v2, v1 or batch answer, with its byte range.
fn emitted_urls(answer: &Value) -> Vec<(String, (u64, u64))> {
    let mut urls = Vec::new();
    for entries in answer["xorbs"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        for entry in entries.as_array().unwrap() {
            urls.push((
                entry["url"].as_str().unwrap().to_owned(),
                range_of(&entry["ranges"][0]["bytes"]),
            ));
        }
    }
    for entries in answer["fetch_info"]
        .as_object()
        .into_iter()
        .flat_map(|m| m.values())
    {
        for entry in entries.as_array().unwrap() {
            urls.push((
                entry["url"].as_str().unwrap().to_owned(),
                range_of(&entry["url_range"]),
            ));
        }
    }
    urls
}

/// The bytes `start..=end` of the stored xorb `xorb`, read from the bucket.
async fn stored_range(server: &TestServer, xorb: MerkleHash, start: u64, end: u64) -> Vec<u8> {
    let bucket = Arc::clone(&server.bucket);
    server
        .on_server(async move {
            bucket
                .get_range(&openxet_storage::Key::Xorb(xorb), start, end)
                .await
                .unwrap()
                .to_vec()
        })
        .await
}

/// The xorb a transfer URL names.
fn url_xorb(url: &str) -> MerkleHash {
    let url = Url::parse(url).unwrap();
    hash_from_hex(url.path().rsplit('/').next().unwrap()).unwrap()
}

#[tokio::test]
async fn hf_xet_capture_downloads_byte_exact() {
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;

    for (file, expected) in capture_files() {
        if expected.is_empty() {
            reconstruct(&server, "v2", &file, Some(FIRST_BLOCK), &server.read_token)
                .await
                .assert_error(416, "range_not_satisfiable");
            continue;
        }
        let got = download(&server, &file, Some(FIRST_BLOCK), expected.len()).await;
        assert_eq!(got.len(), expected.len());
        assert!(got == expected, "{} differs", hash_to_hex(&file));
    }
}

/// Registers the empty file, whose hash is all zeros, to scope A.
async fn register_empty_file(server: &TestServer, vis: Visibility) -> MerkleHash {
    let empty = MerkleHash::default();
    let index = Arc::clone(&server.index);
    let file = NewFile {
        record: FileRecord {
            hash: empty,
            size: 0,
            terms: vec![],
        },
        sha256: None,
        first_chunk: None,
    };
    server
        .on_server(async move {
            index
                .register_shard(&chunk_hash(b"empty"), &[file], SCOPE_A, vis)
                .await
                .unwrap()
        })
        .await;
    empty
}

#[tokio::test]
async fn reconstruction_range_edges() {
    let server = TestServer::start_default().await;
    let vis = Visibility::Global;
    let chunks = five_chunks();
    let file = synthetic_file(
        &server,
        &server.write_token,
        SCOPE_A,
        vis,
        &chunks,
        &[(0, 2), (2, 5)],
    )
    .await;
    let size = file.bytes.len() as u64;
    let single = synthetic_file(
        &server,
        &server.write_token,
        SCOPE_A,
        vis,
        &[vec![42]],
        &[(0, 1)],
    )
    .await;
    let empty = register_empty_file(&server, vis).await;

    // Starts inside chunk 1 (bytes 7..18): the first term is trimmed to
    // start at that chunk, and the offset skips into it.
    let reply = reconstruct(
        &server,
        "v2",
        &file.hash,
        Some("bytes=12-40"),
        &server.read_token,
    )
    .await;
    let json = reply.json();
    assert_eq!(json["offset_into_first_range"], 5);
    assert_eq!(json["terms"][0]["range"], json!({"start": 1, "end": 2}));
    // Mid-chunk starts, ends past EOF (clamped), open-ended and suffix
    // ranges as upstream's reference server serves them, and no `Range`.
    let len = file.bytes.len();
    for (range, from, to) in [
        (Some("bytes=12-40"), 12, 41),
        (Some("bytes=3-100000"), 3, len),
        (Some(FIRST_BLOCK), 0, len),
        (Some("bytes=50-"), 50, len),
        (Some("bytes=-4"), len - 4, len),
        (None, 0, len),
    ] {
        let got = download(&server, &file.hash, range, to - from).await;
        assert_eq!(got, file.bytes[from..to], "{range:?}");
    }

    // A start at or past EOF is `416`, which the client reads as the end.
    for range in [
        format!("bytes={size}-{}", size + 10),
        format!("bytes={}-", size + 1),
    ] {
        for version in ["v1", "v2"] {
            let reply = reconstruct(
                &server,
                version,
                &file.hash,
                Some(&range),
                &server.read_token,
            )
            .await;
            reply.assert_error(416, "range_not_satisfiable");
            assert_eq!(reply.headers[CONTENT_RANGE], format!("bytes */{size}"));
        }
    }

    // The empty file: `416` for any range, as the spec says; without a
    // `Range`, an answer with no terms.
    for range in [FIRST_BLOCK, "bytes=0-0", "bytes=-1"] {
        let reply = reconstruct(&server, "v2", &empty, Some(range), &server.read_token).await;
        reply.assert_error(416, "range_not_satisfiable");
        assert_eq!(reply.headers[CONTENT_RANGE], "bytes */0");
    }
    let whole = reconstruct(&server, "v2", &empty, None, &server.read_token).await;
    assert_eq!(whole.status, 200, "{}", whole.body);
    assert_eq!(
        whole.json(),
        json!({"offset_into_first_range": 0, "terms": [], "xorbs": {}})
    );

    // A single-byte file.
    for range in [FIRST_BLOCK, "bytes=0-0"] {
        assert_eq!(download(&server, &single.hash, Some(range), 1).await, [42]);
    }
    reconstruct(
        &server,
        "v2",
        &single.hash,
        Some("bytes=1-1"),
        &server.read_token,
    )
    .await
    .assert_error(416, "range_not_satisfiable");
}

#[tokio::test]
async fn malformed_range_is_400() {
    let server = TestServer::start_default().await;
    let file = synthetic_file(
        &server,
        &server.write_token,
        SCOPE_A,
        Visibility::Global,
        &five_chunks(),
        &[(0, 5)],
    )
    .await;

    // Malformed ranges are the client's bug, not the end of the file.
    for range in [
        "bytes=5-2",
        "items=0-1",
        "bytes=0-1,4-5",
        "bytes=x-1",
        "bytes=-",
    ] {
        reconstruct(&server, "v2", &file.hash, Some(range), &server.read_token)
            .await
            .assert_error(400, "invalid_range");
    }
}

#[tokio::test]
async fn v1_and_v2_agree() {
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;
    // Two xorbs, with a term that goes back to an earlier chunk range.
    let vis = Visibility::Global;
    let chunks = five_chunks();
    let multi = synthetic_file(
        &server,
        &server.write_token,
        SCOPE_A,
        vis,
        &chunks,
        &[(0, 2), (2, 4), (0, 1), (4, 5)],
    )
    .await;
    let mut files: Vec<MerkleHash> = capture_files()
        .into_iter()
        .filter(|(_, bytes)| !bytes.is_empty())
        .map(|(hash, _)| hash)
        .collect();
    files.push(multi.hash);

    for file in &files {
        for range in [
            None,
            Some(FIRST_BLOCK),
            Some("bytes=10-20"),
            Some("bytes=-3"),
        ] {
            let v2 = reconstruct(&server, "v2", file, range, &server.read_token).await;
            let v1 = reconstruct(&server, "v1", file, range, &server.read_token).await;
            assert_eq!(
                (v2.status.as_u16(), v1.status.as_u16()),
                (200, 200),
                "{} {}",
                v2.body,
                v1.body
            );
            assert_eq!(v1.headers[CACHE_CONTROL], "no-store");
            let (v2, v1) = (v2.json(), v1.json());
            assert_eq!(v1["offset_into_first_range"], v2["offset_into_first_range"]);
            assert_eq!(v1["terms"], v2["terms"]);
            // Each v1 entry is one v2 descriptor, with the same URL: both are
            // signed at the same instant.
            let as_v2: serde_json::Map<String, Value> = v1["fetch_info"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(xorb, entries)| {
                    let entries = entries
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|e| {
                            assert_eq!(e.as_object().unwrap().len(), 3, "{e}");
                            json!({"url": e["url"], "ranges": [{"chunks": e["range"], "bytes": e["url_range"]}]})
                        })
                        .collect();
                    (xorb.clone(), Value::Array(entries))
                })
                .collect();
            assert_eq!(Value::Object(as_v2), v2["xorbs"]);
        }
    }

    // Consecutive terms over adjacent chunks share one descriptor; a term
    // that goes back gets its own.
    let reply = reconstruct(&server, "v2", &multi.hash, None, &server.read_token).await;
    let json = reply.json();
    let xorb = &json["terms"][0]["hash"];
    let ranges: Vec<Value> = json["xorbs"][xorb.as_str().unwrap()]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["ranges"][0]["chunks"].clone())
        .collect();
    assert_eq!(
        ranges,
        [
            json!({"start": 0, "end": 4}),
            json!({"start": 0, "end": 1}),
            json!({"start": 4, "end": 5})
        ]
    );
    assert_eq!(
        download(&server, &multi.hash, None, multi.bytes.len()).await,
        multi.bytes
    );
}

#[tokio::test]
async fn refreshed_reconstruction_keeps_terms_and_ranges() {
    // ADR 0002 §4: a refresh after a 403 gets the same terms and per-entry
    // ranges, with fresh URLs.
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;
    let file = hf_xet_files()[2];

    let first = reconstruct(&server, "v2", &file, Some(FIRST_BLOCK), &server.read_token).await;
    server.clock.advance(1);
    let second = reconstruct(&server, "v2", &file, Some(FIRST_BLOCK), &server.read_token).await;
    assert_eq!((first.status.as_u16(), second.status.as_u16()), (200, 200));
    let (first, second) = (first.json(), second.json());
    assert!(!emitted_urls(&first).is_empty());

    assert_eq!(first["terms"], second["terms"]);
    assert_eq!(
        first["offset_into_first_range"],
        second["offset_into_first_range"]
    );
    let ranges = |answer: &Value| -> Vec<(u64, u64)> {
        emitted_urls(answer).into_iter().map(|(_, r)| r).collect()
    };
    assert_eq!(ranges(&first), ranges(&second));
    let urls = |answer: &Value| -> BTreeSet<String> {
        emitted_urls(answer).into_iter().map(|(u, _)| u).collect()
    };
    assert!(
        urls(&first).is_disjoint(&urls(&second)),
        "the URLs were not refreshed"
    );
}

fn batch_get_path(files: &[MerkleHash]) -> String {
    let query: Vec<String> = files
        .iter()
        .map(|f| format!("file_id={}", hash_to_hex(f)))
        .collect();
    format!("/v1/reconstructions?{}", query.join("&"))
}

fn batch_body(files: &[MerkleHash]) -> String {
    let keys: Vec<Value> = files
        .iter()
        .map(|f| json!({"prefix": "default", "hash": hash_to_hex(f)}))
        .collect();
    Value::Array(keys).to_string()
}

async fn batch_get(server: &TestServer, files: &[MerkleHash], token: &str) -> Reply {
    Reply::of(
        server
            .request(Method::GET, &batch_get_path(files))
            .header(AUTHORIZATION, bearer(token)),
    )
    .await
}

async fn batch_post(server: &TestServer, body: String, token: &str) -> Reply {
    Reply::of(
        server
            .request(Method::POST, BATCH_POST_PATH)
            .header(AUTHORIZATION, bearer(token))
            .header(CONTENT_TYPE, "application/json")
            .body(body),
    )
    .await
}

#[tokio::test]
async fn batch_both_forms_agree() {
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;
    let [empty, a, b] = hf_xet_files()[..] else {
        unreachable!()
    };
    let unknown = chunk_hash(b"never registered");
    let asked = [a, unknown, b, a, empty];

    let get = batch_get(&server, &asked, &server.read_token).await;
    let post = batch_post(&server, batch_body(&asked), &server.read_token).await;

    assert_eq!(
        (get.status.as_u16(), post.status.as_u16()),
        (200, 200),
        "{} {}",
        get.body,
        post.body
    );
    for reply in [&get, &post] {
        assert_eq!(reply.headers[CONTENT_TYPE], "application/json");
        assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
    }
    let (get, post) = (get.json(), post.json());
    assert_eq!(get, post);
    let files: BTreeSet<&str> = get["files"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let expected: BTreeSet<String> = [empty, a, b].iter().map(hash_to_hex).collect();
    assert_eq!(
        files,
        expected.iter().map(String::as_str).collect(),
        "an unknown file is left out"
    );
    assert_eq!(get["files"][hash_to_hex(&empty)], json!([]));

    // Each file's terms are its whole-file v1 terms, and every term lies in
    // one entry of the shared `fetch_info`.
    for file in [a, b] {
        let v1 = reconstruct(&server, "v1", &file, None, &server.read_token)
            .await
            .json();
        let terms = &get["files"][hash_to_hex(&file)];
        assert_eq!(terms, &v1["terms"]);
        for term in terms.as_array().unwrap() {
            let (s, e) = range_of(&term["range"]);
            assert!(
                get["fetch_info"][term["hash"].as_str().unwrap()]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|entry| {
                        let (es, ee) = range_of(&entry["range"]);
                        es <= s && e <= ee
                    }),
                "{term}"
            );
        }
    }
    for (url, (s, e)) in emitted_urls(&get) {
        let got = fetch(&server, &url, Some(format!("bytes={s}-{e}"))).await;
        assert_eq!(got.status, 206);
        assert_eq!(got.body, stored_range(&server, url_xorb(&url), s, e).await);
    }
}

#[tokio::test]
async fn batch_is_capped_and_validated() {
    let server = TestServer::start_default().await;
    let files = |n: usize| -> Vec<MerkleHash> {
        (0..n)
            .map(|i: usize| chunk_hash(&i.to_le_bytes()))
            .collect()
    };

    assert_eq!(
        batch_post(
            &server,
            batch_body(&files(MAX_BATCH_FILES)),
            &server.read_token
        )
        .await
        .status,
        200
    );
    batch_post(
        &server,
        batch_body(&files(MAX_BATCH_FILES + 1)),
        &server.read_token,
    )
    .await
    .assert_error(400, "batch_too_large");
    // Repeats count once.
    let mut repeated = files(MAX_BATCH_FILES);
    repeated.extend(files(10));
    assert_eq!(
        batch_post(&server, batch_body(&repeated), &server.read_token)
            .await
            .status,
        200
    );
    // A GET's query is bounded by the URI length clients and hyper accept
    // (64 KiB), about 880 ids, before the cap is reached.
    assert_eq!(
        batch_get(&server, &files(800), &server.read_token)
            .await
            .status,
        200
    );

    let hash = hash_to_hex(&MerkleHash::default());
    for body in [
        String::new(),
        "{}".to_owned(),
        format!(r#"[{{"hash":"{hash}"}}]"#),
        format!(r#"[{{"prefix":"other","hash":"{hash}"}}]"#),
    ] {
        batch_post(&server, body, &server.read_token)
            .await
            .assert_error(400, "invalid_batch");
    }
    batch_post(
        &server,
        format!(
            r#"[{{"prefix":"default","hash":"{}"}}]"#,
            hash.to_uppercase().replace('0', "A")
        ),
        &server.read_token,
    )
    .await
    .assert_error(400, "invalid_hash");
    Reply::of(
        server
            .request(Method::GET, "/v1/reconstructions?file_id=xyz")
            .header(AUTHORIZATION, bearer(&server.read_token)),
    )
    .await
    .assert_error(400, "invalid_hash");
}

#[tokio::test]
async fn head_file_size() {
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;

    for (file, bytes) in capture_files() {
        let reply = Reply::of(
            server
                .request(Method::HEAD, &format!("/v1/files/{}", hash_to_hex(&file)))
                .header(AUTHORIZATION, bearer(&server.read_token)),
        )
        .await;
        assert_eq!(reply.status, 200);
        assert_eq!(reply.headers[CONTENT_LENGTH], bytes.len().to_string());
        assert!(reply.body.is_empty());
    }
    // The capture's empty file is the all-zero hash, so another one.
    let never = chunk_hash(b"never registered");
    let missing = Reply::of(
        server
            .request(Method::HEAD, &format!("/v1/files/{}", hash_to_hex(&never)))
            .header(AUTHORIZATION, bearer(&server.read_token)),
    )
    .await;
    missing.assert_error(404, "not_found");
}

#[tokio::test]
async fn file_registered_elsewhere_is_404() {
    // Files are per scope under every visibility (design §5.4, ruling
    // M1-R16): another scope's file reads exactly as one never stored.
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;
    let file = hf_xet_files()[1];
    let never = chunk_hash(b"never registered");

    for token in [&server.other_scope_token, &server.other_scope_read_token] {
        for version in ["v2", "v1"] {
            let foreign = reconstruct(&server, version, &file, Some(FIRST_BLOCK), token).await;
            let missing = reconstruct(&server, version, &never, Some(FIRST_BLOCK), token).await;
            foreign.assert_error(404, "not_found");
            assert_eq!(foreign.json(), missing.json());
            assert!(
                !foreign.body.contains(&hash_to_hex(&file)),
                "{}",
                foreign.body
            );
        }
        let head = Reply::of(
            server
                .request(Method::HEAD, &format!("/v1/files/{}", hash_to_hex(&file)))
                .header(AUTHORIZATION, bearer(token)),
        )
        .await;
        head.assert_error(404, "not_found");
        let batch = batch_get(&server, &[file], token).await;
        assert_eq!(batch.json(), json!({"files": {}, "fetch_info": {}}));
    }
}

/// A request for `route` aimed at scope A's data: the capture's xorb, its
/// file `a.bin`, that file's first chunk and its shard.
fn aimed_at_scope_a(route: &Route) -> (Method, String, Vec<u8>) {
    let [_, a, b] = hf_xet_files()[..] else {
        unreachable!()
    };
    let xorb = hash_from_hex(HF_XET_XORB).unwrap();
    let first_chunk = {
        let (data, offsets) = decode_fetched_chunks(&hf_xet_xorb()).unwrap();
        let seg = read_upload_shard(&hf_xet_shard()[..]).unwrap().files[1].segments[0];
        let start = seg.chunk_start as usize;
        chunk_hash(&data[offsets[start] as usize..offsets[start + 1] as usize])
    };
    let method = route.method.clone();
    let (path, body) = match (route.method.as_str(), route.path) {
        ("POST", "/v1/telemetry") => (route.path.to_owned(), b"{}".to_vec()),
        // An upload's body is empty: with another scope's write token,
        // nothing an upload could do reaches scope A.
        ("POST" | "HEAD", XORB_PATH) => (xorb_path(&xorb), vec![]),
        ("POST", "/v1/shards" | "/v2/shards") => (route.path.to_owned(), hf_xet_shard()),
        ("GET", DEDUP_PATH) => (
            format!("/v1/chunks/default/{}", hash_to_hex(&first_chunk)),
            vec![],
        ),
        ("GET", DEDUP_MERKLEDB_PATH) => (
            format!("/v1/chunks/default-merkledb/{}", hash_to_hex(&first_chunk)),
            vec![],
        ),
        ("GET", RECONSTRUCTION_V2_PATH) => {
            (format!("/v2/reconstructions/{}", hash_to_hex(&a)), vec![])
        }
        ("GET", RECONSTRUCTION_V1_PATH) => {
            (format!("/v1/reconstructions/{}", hash_to_hex(&a)), vec![])
        }
        ("GET", BATCH_GET_PATH) => (batch_get_path(&[a, b]), vec![]),
        ("POST", BATCH_POST_PATH) => (route.path.to_owned(), batch_body(&[a, b]).into_bytes()),
        ("HEAD", FILE_PATH) => (format!("/v1/files/{}", hash_to_hex(&a)), vec![]),
        _ => panic!(
            "add a scope-A case for {} {} to credential_scoped_to_repo",
            route.method, route.path
        ),
    };
    (method, path, body)
}

async fn send_aimed(server: &TestServer, route: &Route, token: Option<&str>) -> Reply {
    let (method, path, body) = aimed_at_scope_a(route);
    let mut request = server
        .request(method, &path)
        .header(RANGE, FIRST_BLOCK)
        .body(body);
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, bearer(token));
    }
    Reply::of(request).await
}

#[tokio::test]
async fn credential_scoped_to_repo() {
    // Design §5.9 req 2, the CAS half. Under per-scope visibility, scope B's
    // read and write tokens, on every route and aimed at scope A's data:
    // - a write route with the read token is `403`
    // - a read of A's xorb, chunk or file is `404`, and a batch leaves A's
    //   files out
    // - a write with the write token lands in scope B only, so a shard
    //   naming A's xorb is refused as naming an unknown xorb
    // and no answer carries A's data. Scope A's own read token is the
    // control: the same requests succeed for it.
    let server = TestServer::start(config(Visibility::PerScope)).await;
    upload_capture(&server, &server.write_token).await;
    let unknown = "is not stored on this server";

    for route in ROUTES {
        if route.access == Access::Read {
            let own = send_aimed(&server, route, Some(&server.read_token)).await;
            assert!(
                own.status.is_success(),
                "{} {}: {}",
                route.method,
                route.path,
                own.status
            );
        }
        for (token, write) in [
            (&server.other_scope_read_token, false),
            (&server.other_scope_token, true),
        ] {
            let reply = send_aimed(&server, route, Some(token)).await;
            let what = format!("{} {} write={write}", route.method, route.path);
            match (route.access, route.method.as_str(), route.path) {
                (Access::Write, _, _) if !write => reply.assert_error(403, "insufficient_scope"),
                (_, "POST", "/v1/telemetry") => assert_eq!(reply.status, 204, "{what}"),
                (_, "POST", XORB_PATH) => reply.assert_error(400, "xorb_empty"),
                (_, "POST", "/v1/shards") => {
                    reply.assert_error(400, "unknown_xorb");
                    assert!(reply.body.contains(unknown), "{what}: {}", reply.body);
                }
                (_, "POST", "/v2/shards") => {
                    assert_eq!(reply.status, 200, "{what}");
                    let lines = lines(&reply.body);
                    let Event::Error { message, .. } = &terminal(&lines).event else {
                        panic!("{what}: {lines:?}")
                    };
                    assert!(message.contains(unknown), "{what}: {message}");
                }
                (_, "GET", BATCH_GET_PATH) | (_, "POST", BATCH_POST_PATH) => {
                    assert_eq!(reply.status, 200, "{what}");
                    assert_eq!(
                        reply.json(),
                        json!({"files": {}, "fetch_info": {}}),
                        "{what}"
                    );
                }
                // A `HEAD` answer has no body, so no code to check.
                (_, "HEAD", _) => reply.assert_error(404, ""),
                _ => reply.assert_error(404, "not_found"),
            }
            assert!(
                !reply.body.contains("/v1/transfer/"),
                "{what} handed out a data URL: {}",
                reply.body
            );
        }
    }
    // Nothing B did reached scope A: A still reads its file, B still can't.
    let a = hf_xet_files()[1];
    assert_eq!(
        reconstruct(&server, "v2", &a, None, &server.read_token)
            .await
            .status,
        200
    );
    reconstruct(&server, "v2", &a, None, &server.other_scope_read_token)
        .await
        .assert_error(404, "not_found");
}

#[tokio::test]
async fn transfer_requires_signature() {
    // Design §5.9 req 1, ruling M1-R3: the signature is the credential, and
    // it is bound to one xorb, one range and an expiry.
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;
    let answer = reconstruct(&server, "v2", &hf_xet_files()[2], None, &server.read_token)
        .await
        .json();
    let (url, (s, e)) = emitted_urls(&answer).remove(0);
    let exact = || Some(format!("bytes={s}-{e}"));
    let parsed = Url::parse(&url).unwrap();
    let xorb = url_xorb(&url);
    let bare = format!("{}{}", server.base_url, parsed.path());
    assert_eq!(
        parsed.path(),
        format!("/v1/transfer/xorbs/{}", hash_to_hex(&xorb))
    );

    // No query: not a signed URL.
    let got = fetch(&server, &bare, exact()).await;
    assert_eq!(got.status, 400);
    assert!(String::from_utf8_lossy(&got.body).contains("\"invalid_transfer_url\""));

    // A bad signature, another range, another xorb.
    let sig: String = parsed
        .query_pairs()
        .find(|(k, _)| k == "sig")
        .unwrap()
        .1
        .into_owned();
    let flipped = {
        let mut chars: Vec<char> = sig.chars().collect();
        chars[0] = if chars[0] == 'A' { 'B' } else { 'A' };
        chars.into_iter().collect::<String>()
    };
    let forged = url.replace(&format!("sig={sig}"), &format!("sig={flipped}"));
    let other_xorb = url.replace(&hash_to_hex(&xorb), &hash_to_hex(&chunk_hash(b"other")));
    for (url, range) in [
        (forged.as_str(), exact()),
        (url.as_str(), Some(format!("bytes={s}-{}", e - 1))),
        (url.as_str(), Some(format!("bytes={}-{e}", s + 1))),
        (url.as_str(), Some(format!("bytes={s}-"))),
        (other_xorb.as_str(), exact()),
    ] {
        let got = fetch(&server, url, range).await;
        assert_eq!(got.status, 403, "{url}");
        assert!(String::from_utf8_lossy(&got.body).contains("\"forbidden\""));
    }

    // No `Range`: `400`, ADR 0002's third probe check.
    let got = fetch(&server, &url, None).await;
    assert_eq!(got.status, 400);
    assert!(String::from_utf8_lossy(&got.body).contains("\"bad_range\""));

    // The exact range: `206` with exactly the stored bytes. A credential
    // sent along changes nothing: the route never reads it.
    let stored_size = {
        let bucket = Arc::clone(&server.bucket);
        server
            .on_server(async move {
                bucket
                    .size(&openxet_storage::Key::Xorb(xorb))
                    .await
                    .unwrap()
                    .unwrap()
            })
            .await
    };
    for token in [None, Some("oxt_test_unknown")] {
        let mut request = server.http.get(&url).header(RANGE, exact().unwrap());
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, bearer(token));
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 206);
        let headers = response.headers().clone();
        assert_eq!(
            headers[CONTENT_RANGE],
            format!("bytes {s}-{e}/{stored_size}")
        );
        assert_eq!(headers[CONTENT_TYPE], "application/octet-stream");
        assert_eq!(headers[CACHE_CONTROL], "no-store");
        assert_eq!(headers[CONTENT_LENGTH], (e - s + 1).to_string());
        assert!(headers.contains_key("x-request-id"));
        let body = response.bytes().await.unwrap();
        assert_eq!(body.to_vec(), stored_range(&server, xorb, s, e).await);
    }

    // Expired: `403`, on which the client asks for a fresh reconstruction.
    server.clock.advance(UrlSigner::DEFAULT_LIFETIME.as_secs());
    let got = fetch(&server, &url, exact()).await;
    assert_eq!(got.status, 403);
}

#[tokio::test]
async fn transfer_past_the_stored_xorb_is_an_integrity_fault() {
    // The signer only signs ranges inside a stored xorb, so a validly signed
    // URL past its end means the index and the bucket disagree.
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;
    let xorb = hash_from_hex(HF_XET_XORB).unwrap();
    let size = stored(&hf_xet_xorb()).bytes.len() as u64;
    let signer = UrlSigner::new(TRANSFER_KEY, UrlSigner::DEFAULT_LIFETIME).unwrap();
    let now = server.clock.now();
    let base = Url::parse(&server.base_url).unwrap();

    let past = signer
        .sign_xorb_range(&base, &xorb, size - 10, size + 10, now)
        .unwrap();
    let got = fetch(
        &server,
        past.as_str(),
        Some(format!("bytes={}-{}", size - 10, size + 10)),
    )
    .await;
    assert_eq!(got.status, 416);
    assert_eq!(got.headers[CONTENT_RANGE], format!("bytes */{size}"));
    assert!(server.logs().contains("integrity"), "{}", server.logs());

    let absent = chunk_hash(b"absent");
    let missing = signer.sign_xorb_range(&base, &absent, 0, 9, now).unwrap();
    let got = fetch(&server, missing.as_str(), Some("bytes=0-9".to_owned())).await;
    assert_eq!(got.status, 500);
    assert!(String::from_utf8_lossy(&got.body).contains("\"internal\""));
}

#[tokio::test]
async fn no_unauthenticated_reads() {
    // Design §5.9 req 1: every route and every URL the server emits,
    // requested without a credential, is refused, except a signed URL with
    // exactly its range.
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;

    for route in ROUTES {
        send_aimed(&server, route, None)
            .await
            .assert_error(401, "missing_credential");
        let mut plain = server.request(route.method.clone(), route.path);
        if route.path == BATCH_POST_PATH {
            plain = plain.body("[]");
        }
        Reply::of(plain)
            .await
            .assert_error(401, "missing_credential");
    }
    let got = fetch(
        &server,
        &server.url(TRANSFER_PATH),
        Some("bytes=0-0".to_owned()),
    )
    .await;
    assert_eq!(got.status, 400);

    // Collect every URL the server emits, from every reconstruction form.
    let files = hf_xet_files();
    let mut answers = Vec::new();
    for file in &files[1..] {
        for version in ["v2", "v1"] {
            for range in [None, Some(FIRST_BLOCK), Some("bytes=100-200")] {
                answers.push(reconstruct(&server, version, file, range, &server.read_token).await);
            }
        }
    }
    answers.push(batch_get(&server, &files, &server.read_token).await);
    answers.push(batch_post(&server, batch_body(&files), &server.read_token).await);
    let mut urls = Vec::new();
    for answer in &answers {
        assert_eq!(answer.status, 200, "{}", answer.body);
        urls.extend(emitted_urls(&answer.json()));
    }
    assert!(urls.len() > 10);

    for (url, (s, e)) in &urls {
        let parsed = Url::parse(url).unwrap();
        assert_eq!(
            parsed.origin(),
            Url::parse(&server.base_url).unwrap().origin()
        );
        assert!(parsed.path().starts_with("/v1/transfer/xorbs/"), "{url}");
        let keys: Vec<String> = parsed.query_pairs().map(|(k, _)| k.into_owned()).collect();
        assert_eq!(keys, ["range", "exp", "sig"], "{url}");

        assert_eq!(fetch(&server, url, None).await.status, 400, "{url}");
        let other = if *s > 0 {
            format!("bytes=0-{e}")
        } else {
            format!("bytes={s}-{}", e + 1)
        };
        assert_eq!(fetch(&server, url, Some(other)).await.status, 403, "{url}");
        let got = fetch(&server, url, Some(format!("bytes={s}-{e}"))).await;
        assert_eq!(got.status, 206, "{url}");
        assert_eq!(got.body, stored_range(&server, url_xorb(url), *s, *e).await);
    }
}

#[tokio::test]
async fn signed_urls_are_never_logged() {
    // A URL's `sig` is a credential (M1-R20): logs record the route
    // template only.
    let server = TestServer::start_default().await;
    upload_capture(&server, &server.write_token).await;
    let answer = reconstruct(&server, "v2", &hf_xet_files()[2], None, &server.read_token)
        .await
        .json();

    let mut sigs = Vec::new();
    for (url, (s, e)) in emitted_urls(&answer) {
        assert_eq!(
            fetch(&server, &url, Some(format!("bytes={s}-{e}")))
                .await
                .status,
            206
        );
        assert_eq!(
            fetch(&server, &url, Some("bytes=0-0".to_owned()))
                .await
                .status,
            403
        );
        let parsed = Url::parse(&url).unwrap();
        sigs.push(
            parsed
                .query_pairs()
                .find(|(k, _)| k == "sig")
                .unwrap()
                .1
                .into_owned(),
        );
    }

    let logs = server.logs();
    assert!(logs.contains("route=/v1/transfer/xorbs/{hash}"), "{logs}");
    for sig in sigs {
        assert!(!logs.contains(&sig), "a signature was logged");
    }
}

#[tokio::test]
async fn missing_chunk_table_is_500_and_logged() {
    // Registration checked every term's xorb (M1-R17), so a table missing
    // at read time is an index fault: no partial plan is answered.
    let server =
        TestServer::start_with_index(config(Visibility::Global), TablelessIndex::wrap()).await;
    let file = synthetic_file(
        &server,
        &server.write_token,
        SCOPE_A,
        Visibility::Global,
        &five_chunks(),
        &[(0, 5)],
    )
    .await;

    for version in ["v2", "v1"] {
        reconstruct(&server, version, &file.hash, None, &server.read_token)
            .await
            .assert_error(500, "internal");
    }
    let batch = batch_get(&server, &[file.hash], &server.read_token).await;
    batch.assert_error(500, "internal");
    let logs = server.logs();
    assert!(logs.contains("ERROR"), "{logs}");
    assert!(logs.contains("integrity anomaly"), "{logs}");
}

#[tokio::test]
async fn reconstruction_paths_are_checked_before_authentication() {
    let server = TestServer::start_default().await;

    for path in [
        "/v2/reconstructions/XYZ",
        "/v1/reconstructions/0000",
        "/v1/files/zz",
        "/v1/transfer/xorbs/12",
    ] {
        let method = if path.starts_with("/v1/files") {
            Method::HEAD
        } else {
            Method::GET
        };
        Reply::of(server.request(method, path))
            .await
            .assert_error(400, "invalid_hash");
    }
    let encoded = format!("/v2/reconstructions/%30{}", "0".repeat(63));
    Reply::of(server.request(Method::GET, &encoded))
        .await
        .assert_error(400, "invalid_path");
}
