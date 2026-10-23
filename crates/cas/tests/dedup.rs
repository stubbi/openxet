//! `GET /v1/chunks/{prefix}/{chunk}`, global dedup (S10 §2; spec
//! <https://huggingface.co/docs/xet/api#2-query-chunk-deduplication-global-deduplication>
//! and <https://huggingface.co/docs/xet/shard#global-deduplication>;
//! design §5.9 req 3 and 4).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::Arc;

use common::shard::hf_xet_shard;
use common::xorb::{HF_XET_XORB, chunk_data, hf_xet_xorb, stored, xorb_of, xorb_path};
use common::{
    DEDUP_MERKLEDB_PATH, DEDUP_PATH, OTHER_PREFIX_DEDUP_PATH, Reply, TestServer, WrapIndex, bearer,
    config,
};
use openxet_cas::Clock as _;
use openxet_index::{ChunkTable, FileRecord, Index, IndexError, NewFile, Visibility};
use openxet_protocol::{
    DedupAnswer, MerkleHash, chunk_hash, hash_to_hex, is_dedup_eligible, read_dedup_answer,
    read_upload_shard,
};
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE};

/// The key lifetime ruling M1-R4 sets: 7 days.
const KEY_LIFETIME: u64 = 7 * 24 * 60 * 60;

/// A dedup answer, its body kept as bytes.
struct Answer {
    status: u16,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
}

fn dedup_path(prefix: &str, chunk: &MerkleHash) -> String {
    format!("/v1/chunks/{prefix}/{}", hash_to_hex(chunk))
}

/// `GET path` with `token`, as `hf_xet` queries: no body.
async fn get(server: &TestServer, path: &str, token: Option<&str>) -> Answer {
    let mut request = server.request(Method::GET, path);
    if let Some(token) = token {
        request = request.header(AUTHORIZATION, bearer(token));
    }
    let response = request.send().await.unwrap();
    Answer {
        status: response.status().as_u16(),
        headers: response.headers().clone(),
        body: response.bytes().await.unwrap().to_vec(),
    }
}

async fn query(server: &TestServer, chunk: &MerkleHash, token: &str) -> Answer {
    get(server, &dedup_path("default", chunk), Some(token)).await
}

/// The same answer as a [`Reply`], for error assertions.
fn reply(answer: &Answer) -> Reply {
    Reply {
        status: reqwest::StatusCode::from_u16(answer.status).unwrap(),
        headers: answer.headers.clone(),
        body: String::from_utf8(answer.body.clone()).unwrap(),
        head: false,
    }
}

/// Asserts a `200` dedup answer offering `xorb` for `chunk`, as the
/// client reads it, and returns it.
fn assert_offers(answer: &Answer, chunk: &MerkleHash, xorb: &MerkleHash) -> DedupAnswer {
    assert_eq!(
        answer.status,
        200,
        "{}",
        String::from_utf8_lossy(&answer.body)
    );
    assert_eq!(answer.headers[CONTENT_TYPE], "application/octet-stream");
    let read = read_dedup_answer(&answer.body, chunk).unwrap();
    assert_eq!(read.files, 0, "a dedup answer carries no file info");
    assert_eq!(read.xorbs, vec![*xorb]);
    assert_eq!(read.matched.map(|(x, _)| x), Some(*xorb));
    assert_ne!(read.key, MerkleHash::default());
    read
}

fn assert_no_match(answer: &Answer) {
    reply(answer).assert_error(404, "not_found");
}

async fn upload_xorb(server: &TestServer, body: Vec<u8>, token: &str) -> MerkleHash {
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

async fn upload_shard(server: &TestServer, token: &str) {
    let reply = Reply::of(
        server
            .request(Method::POST, "/v1/shards")
            .header(AUTHORIZATION, bearer(token))
            .body(hf_xet_shard()),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

/// A xorb of one eligible and one ineligible chunk, and their hashes.
fn mixed_xorb() -> (Vec<u8>, MerkleHash, MerkleHash) {
    let (eligible, other) = (chunk_data(true), chunk_data(false));
    let body = xorb_of(&[&eligible, &other]);
    (body, chunk_hash(&eligible), chunk_hash(&other))
}

/// The first chunk of the capture's `a.bin`: the chunk its first segment
/// starts at.
fn capture_first_chunk() -> MerkleHash {
    let shard = read_upload_shard(&hf_xet_shard()[..]).unwrap();
    let segment = &shard.files[1].segments[0];
    assert_eq!(hash_to_hex(&segment.xorb), HF_XET_XORB);
    stored(&hf_xet_xorb()).chunks.hashes[segment.chunk_start as usize].0
}

/// Whether `body` holds `chunk` as 32 bytes or as an 8-byte truncation.
fn reveals(body: &[u8], chunk: &MerkleHash) -> bool {
    let raw = chunk.as_bytes();
    body.windows(32).any(|w| w == raw) || body.windows(8).any(|w| w == &raw[..8])
}

#[tokio::test]
async fn both_prefixes_answer() {
    // `hf_xet` sends `default`; the spec names `default-merkledb` (S10 §2).
    let server = TestServer::start_default().await;
    let (body, eligible, _) = mixed_xorb();
    let xorb = upload_xorb(&server, body, &server.write_token).await;

    for prefix in ["default", "default-merkledb"] {
        let answer = get(
            &server,
            &dedup_path(prefix, &eligible),
            Some(&server.read_token),
        )
        .await;
        assert_offers(&answer, &eligible, &xorb);
    }
    for path in [DEDUP_PATH, DEDUP_MERKLEDB_PATH] {
        assert_no_match(&get(&server, path, Some(&server.read_token)).await);
    }
    assert_no_match(&get(&server, OTHER_PREFIX_DEDUP_PATH, Some(&server.read_token)).await);
    reply(
        &get(
            &server,
            &format!("/v1/chunks/default/{}", "A".repeat(64)),
            Some(&server.read_token),
        )
        .await,
    )
    .assert_error(400, "invalid_hash");
}

#[tokio::test]
async fn dedup_response_hmac_keyed() {
    // §5.9 req 4: a dedup answer never exposes a raw chunk hash.
    let server = TestServer::start_default().await;
    let (body, eligible, other) = mixed_xorb();
    let xorb = upload_xorb(&server, body, &server.write_token).await;

    let first = query(&server, &eligible, &server.read_token).await;
    let second = query(&server, &eligible, &server.read_token).await;

    let keys = [
        assert_offers(&first, &eligible, &xorb).key,
        assert_offers(&second, &eligible, &xorb).key,
    ];
    for answer in [&first, &second] {
        for chunk in [&eligible, &other] {
            assert!(!reveals(&answer.body, chunk), "raw chunk hash {chunk} sent");
        }
    }
    assert_ne!(keys[0], keys[1]);
}

#[tokio::test]
async fn first_chunk_of_file_is_queryable() {
    // The client queries each file's first chunk whatever its hash (S10
    // §2), so shard registration records it (Task 9).
    let server = TestServer::start_default().await;
    let first = capture_first_chunk();
    assert!(
        !is_dedup_eligible(&first),
        "the test needs a first chunk only the shard makes queryable"
    );
    let xorb = upload_xorb(&server, hf_xet_xorb(), &server.write_token).await;
    assert_no_match(&query(&server, &first, &server.read_token).await);

    upload_shard(&server, &server.write_token).await;

    let read = assert_offers(
        &query(&server, &first, &server.read_token).await,
        &first,
        &xorb,
    );
    let shard = read_upload_shard(&hf_xet_shard()[..]).unwrap();
    assert_eq!(
        read.matched,
        Some((xorb, shard.files[1].segments[0].chunk_start))
    );
}

#[tokio::test]
async fn non_eligible_chunk_is_404() {
    let server = TestServer::start_default().await;
    let (body, _, other) = mixed_xorb();
    upload_xorb(&server, body, &server.write_token).await;

    let answer = query(&server, &other, &server.read_token).await;

    assert_no_match(&answer);
}

#[tokio::test]
async fn per_scope_dedup_isolates_tenants() {
    // §5.9 req 4: under `PerScope`, another scope's chunk answers exactly as
    // one never stored.
    let isolated = TestServer::start(config(Visibility::PerScope)).await;
    let (body, eligible, _) = mixed_xorb();
    let xorb = upload_xorb(&isolated, body.clone(), &isolated.write_token).await;

    assert_offers(
        &query(&isolated, &eligible, &isolated.read_token).await,
        &eligible,
        &xorb,
    );
    let foreign = query(&isolated, &eligible, &isolated.other_scope_token).await;
    let never = query(
        &isolated,
        &MerkleHash::default(),
        &isolated.other_scope_token,
    )
    .await;
    assert_no_match(&foreign);
    assert_eq!(foreign.body, never.body);
    let names = |answer: &Answer| -> HashMap<String, String> {
        answer
            .headers
            .iter()
            .filter(|(name, _)| !matches!(name.as_str(), "x-request-id" | "date"))
            .map(|(n, v)| (n.to_string(), v.to_str().unwrap().to_owned()))
            .collect()
    };
    assert_eq!(names(&foreign), names(&never));

    let shared = TestServer::start(config(Visibility::Global)).await;
    let xorb = upload_xorb(&shared, body, &shared.write_token).await;
    assert_offers(
        &query(&shared, &eligible, &shared.other_scope_token).await,
        &eligible,
        &xorb,
    );
}

#[tokio::test]
async fn unauthenticated_is_401() {
    let server = TestServer::start_default().await;
    let (body, eligible, _) = mixed_xorb();
    upload_xorb(&server, body, &server.write_token).await;

    for prefix in ["default", "default-merkledb"] {
        let path = dedup_path(prefix, &eligible);
        reply(&get(&server, &path, None).await).assert_error(401, "missing_credential");
        reply(&get(&server, &path, Some("oxt_test_unknown")).await)
            .assert_error(401, "invalid_credential");
    }
}

#[tokio::test]
async fn dedup_response_is_cache_control_no_store() {
    // §5.9 req 3: each answer is keyed per request and depends on the
    // caller's authorization, so no cache may keep one, nor the 404 that
    // under `PerScope` another scope gets.
    let server = TestServer::start_default().await;
    let (body, eligible, _) = mixed_xorb();
    upload_xorb(&server, body, &server.write_token).await;

    let hit = query(&server, &eligible, &server.read_token).await;
    let miss = get(&server, DEDUP_PATH, Some(&server.read_token)).await;

    assert_eq!(hit.status, 200);
    assert_eq!(
        hit.headers[CONTENT_LENGTH].to_str().unwrap(),
        hit.body.len().to_string()
    );
    for answer in [&hit, &miss] {
        assert_eq!(answer.headers[CACHE_CONTROL], "no-store");
    }
}

#[tokio::test]
async fn dedup_end_to_end_with_capture() {
    // The `hf_xet` 1.7.0 capture: its xorb, then its shard, then a query for
    // `a.bin`'s first chunk, read back with upstream's readers as the client
    // reads it.
    let server = TestServer::start_default().await;
    let xorb = upload_xorb(&server, hf_xet_xorb(), &server.write_token).await;
    upload_shard(&server, &server.write_token).await;
    let first = capture_first_chunk();
    let now = server.clock.now();

    let answer = query(&server, &first, &server.read_token).await;

    let read = assert_offers(&answer, &first, &xorb);
    assert_eq!(read.expiry_unix, now + KEY_LIFETIME);
    for (chunk, _) in &stored(&hf_xet_xorb()).chunks.hashes {
        assert!(!reveals(&answer.body, chunk), "raw chunk hash {chunk} sent");
    }
    assert!(
        !server.logs().contains(&hash_to_hex(&read.key)),
        "the HMAC key was logged"
    );
}

#[tokio::test]
async fn dedup_responses_use_fresh_keys() {
    let server = TestServer::start_default().await;
    let (body, eligible, _) = mixed_xorb();
    let xorb = upload_xorb(&server, body, &server.write_token).await;

    let first = query(&server, &eligible, &server.read_token).await;
    let second = query(&server, &eligible, &server.read_token).await;

    assert_ne!(
        assert_offers(&first, &eligible, &xorb).key,
        assert_offers(&second, &eligible, &xorb).key
    );
    assert_ne!(first.body, second.body);
}

/// An index whose `chunk_tables` answers nothing, as if the offered xorb's
/// row were gone between the two lookups, or whose `dedup_xorb` fails.
struct BrokenIndex {
    inner: Arc<dyn Index>,
    fail_dedup: bool,
}

impl BrokenIndex {
    fn wrap(fail_dedup: bool) -> WrapIndex {
        Box::new(move |inner| Arc::new(Self { inner, fail_dedup }))
    }
}

#[async_trait::async_trait]
impl Index for BrokenIndex {
    async fn put_xorb(
        &self,
        xorb: &MerkleHash,
        size: u64,
        table: &ChunkTable,
        eligible: &[MerkleHash],
        scope: &str,
    ) -> Result<bool, IndexError> {
        self.inner
            .put_xorb(xorb, size, table, eligible, scope)
            .await
    }

    async fn xorb_size(
        &self,
        xorb: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<u64>, IndexError> {
        self.inner.xorb_size(xorb, scope, vis).await
    }

    async fn chunk_tables(
        &self,
        _: &[MerkleHash],
        _: &str,
        _: Visibility,
    ) -> Result<HashMap<MerkleHash, ChunkTable>, IndexError> {
        Ok(HashMap::new())
    }

    async fn register_shard(
        &self,
        shard: &MerkleHash,
        files: &[NewFile],
        scope: &str,
        vis: Visibility,
    ) -> Result<bool, IndexError> {
        self.inner.register_shard(shard, files, scope, vis).await
    }

    async fn file(&self, file: &MerkleHash, scope: &str) -> Result<Option<FileRecord>, IndexError> {
        self.inner.file(file, scope).await
    }

    async fn dedup_xorb(
        &self,
        chunk: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<MerkleHash>, IndexError> {
        if self.fail_dedup {
            return Err(IndexError::Database(sqlx::Error::PoolTimedOut));
        }
        self.inner.dedup_xorb(chunk, scope, vis).await
    }
}

#[tokio::test]
async fn offered_xorb_without_a_chunk_table_is_404_and_logged() {
    // Nothing deletes in M1, so a xorb the dedup index names without a
    // chunk table is an integrity anomaly; the client still gets a plain
    // "no match".
    let server =
        TestServer::start_with_index(config(Visibility::Global), BrokenIndex::wrap(false)).await;
    let (body, eligible, _) = mixed_xorb();
    let xorb = upload_xorb(&server, body, &server.write_token).await;

    let answer = query(&server, &eligible, &server.read_token).await;

    assert_no_match(&answer);
    let logs = server.logs();
    let anomaly = logs
        .lines()
        .find(|line| line.contains("integrity anomaly"))
        .unwrap_or_else(|| panic!("no integrity anomaly logged: {logs}"));
    assert!(anomaly.contains("WARN"), "{anomaly}");
    assert!(anomaly.contains(&hash_to_hex(&xorb)), "{anomaly}");
}

#[tokio::test]
async fn index_failure_is_503() {
    let server =
        TestServer::start_with_index(config(Visibility::Global), BrokenIndex::wrap(true)).await;

    let answer = get(&server, DEDUP_PATH, Some(&server.read_token)).await;

    reply(&answer).assert_error(503, "unavailable");
}
