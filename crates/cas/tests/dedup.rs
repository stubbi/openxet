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
    DEDUP_MERKLEDB_PATH, DEDUP_PATH, OTHER_PREFIX_DEDUP_PATH, Reply, SCOPE_A, TestServer,
    WrapIndex, bearer, config,
};
use openxet_cas::Clock as _;
use openxet_index::{ChunkTable, FileRecord, Index, IndexError, NewFile, Term, Visibility};
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
/// Upstream's lookup tables truncate a hash to its first little-endian u64
/// word (`truncate_hash`, `xet-core-structures` 1.7.0
/// `metadata_shard/utils.rs:31`), which is `raw[..8]`; no other window
/// can hold it.
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
    // Ruling M1-R4: a key per response, so no two answers can be correlated
    // by their keyed hashes. Sixteen answers, all pairwise distinct.
    let server = TestServer::start_default().await;
    let (body, eligible, _) = mixed_xorb();
    let xorb = upload_xorb(&server, body, &server.write_token).await;

    let mut keys = std::collections::HashSet::new();
    let mut bodies = std::collections::HashSet::new();
    for _ in 0..16 {
        let answer = query(&server, &eligible, &server.read_token).await;
        keys.insert(assert_offers(&answer, &eligible, &xorb).key);
        bodies.insert(answer.body);
    }

    assert_eq!(keys.len(), 16);
    assert_eq!(bodies.len(), 16);
}

#[tokio::test]
async fn percent_encoded_paths_are_400_invalid_path() {
    // One object, one URL (S10 "Conventions for every route"): a decoded
    // `%64efault` or `%37c1...` would name the same object as its plain
    // form.
    let server = TestServer::start_default().await;
    let (body, eligible, _) = mixed_xorb();
    let xorb = upload_xorb(&server, body, &server.write_token).await;
    let (chunk, xorb) = (hash_to_hex(&eligible), hash_to_hex(&xorb));

    for (method, path) in [
        ("GET", format!("/v1/chunks/%64efault/{chunk}")),
        (
            "GET",
            format!(
                "/v1/chunks/default/%{:x}{}",
                chunk.as_bytes()[0],
                &chunk[1..]
            ),
        ),
        ("HEAD", format!("/v1/xorbs/%64efault/{xorb}")),
    ] {
        let head = format!(
            "{method} {path} HTTP/1.1\r\nAuthorization: {}",
            bearer(&server.read_token)
        );
        let answer = server.raw(&head, b"", false).await;
        assert!(answer.starts_with("HTTP/1.1 400"), "{path}: {answer}");
        if method == "GET" {
            assert!(answer.contains("\"invalid_path\""), "{path}: {answer}");
        }
    }
}

#[tokio::test]
async fn another_method_is_405_and_no_store() {
    let server = TestServer::start_default().await;

    let reply = Reply::of(
        server
            .request(Method::POST, DEDUP_PATH)
            .header(AUTHORIZATION, bearer(&server.write_token)),
    )
    .await;

    reply.assert_error(405, "method_not_allowed");
    assert_eq!(reply.headers["allow"], "GET,HEAD");
    assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
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

    async fn dedup_shard_xorbs(
        &self,
        xorb: &MerkleHash,
        scope: &str,
        vis: Visibility,
    ) -> Result<Option<Vec<MerkleHash>>, IndexError> {
        self.inner.dedup_shard_xorbs(xorb, scope, vis).await
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

/// A xorb of `chunks` distinct uncompressed chunks labelled `label`; with
/// `eligible`, its last chunk is the one dedup-eligible chunk
/// [`chunk_data`] finds.
fn labelled_xorb(label: &str, chunks: usize, eligible: bool) -> Vec<u8> {
    let mut data: Vec<Vec<u8>> = (0..chunks)
        .map(|i| format!("{label}/chunk{i}").into_bytes())
        .collect();
    if eligible {
        data.push(chunk_data(true));
    }
    let refs: Vec<&[u8]> = data.iter().map(Vec::as_slice).collect();
    xorb_of(&refs)
}

/// Registers to `scope`, straight in the index, a one-file shard whose
/// terms cover `xorbs` whole in order, as a verified upload would; the
/// file's first chunk becomes a dedup candidate. Returns that chunk.
async fn register_file(
    server: &TestServer,
    label: &str,
    xorbs: &[MerkleHash],
    scope: &'static str,
    vis: Visibility,
) -> MerkleHash {
    let index = Arc::clone(&server.index);
    let xorbs = xorbs.to_vec();
    let shard = chunk_hash(format!("shard/{label}").as_bytes());
    let file = chunk_hash(format!("file/{label}").as_bytes());
    server
        .on_server(async move {
            let tables = index
                .chunk_tables(&xorbs, scope, Visibility::Global)
                .await
                .unwrap();
            let terms: Vec<Term> = xorbs
                .iter()
                .map(|x| {
                    let n = tables[x].hashes.len();
                    Term {
                        xorb: *x,
                        chunk_start: 0,
                        chunk_end: u32::try_from(n).unwrap(),
                        unpacked_bytes: tables[x]
                            .hashes
                            .iter()
                            .map(|(_, l)| u32::try_from(*l).unwrap())
                            .sum(),
                    }
                })
                .collect();
            let first = tables[&xorbs[0]].hashes[0].0;
            let record = FileRecord {
                hash: file,
                size: terms.iter().map(|t| u64::from(t.unpacked_bytes)).sum(),
                terms,
            };
            let new = NewFile {
                record,
                sha256: None,
                first_chunk: Some(first),
            };
            index
                .register_shard(&shard, &[new], scope, vis)
                .await
                .unwrap();
            first
        })
        .await
}

/// The sorted xorbs `answer` offers, read as the client reads it.
fn offered(answer: &Answer, chunk: &MerkleHash) -> DedupAnswer {
    assert_eq!(
        answer.status,
        200,
        "{}",
        String::from_utf8_lossy(&answer.body)
    );
    let mut read = read_dedup_answer(&answer.body, chunk).unwrap();
    read.xorbs.sort();
    read
}

fn sorted(xorbs: &[MerkleHash]) -> Vec<MerkleHash> {
    let mut xorbs = xorbs.to_vec();
    xorbs.sort();
    xorbs
}

#[tokio::test]
async fn dedup_answer_offers_the_shards_xorbs() {
    // Ruling M1-R30: a hit on a file's first chunk offers every xorb of the
    // shard that registered the file, so a client with an empty cache
    // dedups the whole file from one answer (S11).
    let server = TestServer::start_default().await;
    let mut xorbs = Vec::new();
    for (i, eligible) in [false, true, false].into_iter().enumerate() {
        let body = labelled_xorb(&format!("x{i}"), 3, eligible);
        xorbs.push(upload_xorb(&server, body, &server.write_token).await);
    }
    let first = register_file(&server, "f", &xorbs, SCOPE_A, Visibility::Global).await;

    let read = offered(&query(&server, &first, &server.read_token).await, &first);
    assert_eq!(read.xorbs, sorted(&xorbs));
    assert_eq!(read.matched, Some((xorbs[0], 0)));
    assert_eq!(read.files, 0);

    // A hit in the middle xorb offers them all too, matched there.
    let eligible = chunk_hash(&chunk_data(true));
    let read = offered(
        &query(&server, &eligible, &server.read_token).await,
        &eligible,
    );
    assert_eq!(read.xorbs, sorted(&xorbs));
    assert_eq!(read.matched, Some((xorbs[1], 3)));
}

#[tokio::test]
async fn dedup_answer_falls_back_without_a_shard() {
    // A xorb no registered shard references yet is offered alone, as
    // before ruling M1-R30; so is one only another file's shard names.
    let server = TestServer::start_default().await;
    let (body, eligible, _) = mixed_xorb();
    let xorb = upload_xorb(&server, body, &server.write_token).await;
    assert_offers(
        &query(&server, &eligible, &server.read_token).await,
        &eligible,
        &xorb,
    );

    let other = upload_xorb(
        &server,
        labelled_xorb("other", 2, false),
        &server.write_token,
    )
    .await;
    register_file(&server, "other", &[other], SCOPE_A, Visibility::Global).await;
    assert_offers(
        &query(&server, &eligible, &server.read_token).await,
        &eligible,
        &xorb,
    );
}

#[tokio::test]
async fn per_scope_multi_xorb_answer_hides_foreign_xorbs() {
    // §5.9 req 4 under `PerScope`: a shard registered while visibility was
    // `Global` may reference another scope's xorb. Read under `PerScope`,
    // the answer offers only the caller's xorbs and names no other.
    let server = TestServer::start(config(Visibility::PerScope)).await;
    let mine = upload_xorb(
        &server,
        labelled_xorb("mine", 3, false),
        &server.write_token,
    )
    .await;
    let foreign_body = labelled_xorb("foreign", 3, false);
    let foreign_chunks = stored(&foreign_body).chunks.hashes;
    let foreign = upload_xorb(&server, foreign_body, &server.other_scope_token).await;
    let first = register_file(
        &server,
        "mixed",
        &[mine, foreign],
        SCOPE_A,
        Visibility::Global,
    )
    .await;

    let answer = query(&server, &first, &server.read_token).await;
    let read = offered(&answer, &first);
    assert_eq!(read.xorbs, vec![mine]);
    assert!(
        !answer.body.windows(32).any(|w| w == foreign.as_bytes()),
        "the answer names another scope's xorb"
    );
    for (chunk, _) in &*foreign_chunks {
        assert!(!reveals(&answer.body, chunk));
    }

    // Another scope's chunk is still exactly a "never stored".
    let foreign_query = query(&server, &first, &server.other_scope_read_token).await;
    let never = query(
        &server,
        &MerkleHash::default(),
        &server.other_scope_read_token,
    )
    .await;
    assert_no_match(&foreign_query);
    assert_eq!(foreign_query.body, never.body);
}

#[tokio::test]
async fn dedup_answer_never_contains_raw_hashes_or_file_info() {
    // §5.9 req 4 for multi-xorb answers: every chunk hash of every offered
    // xorb is keyed, none appears raw or truncated, and no file is named.
    let server = TestServer::start_default().await;
    let mut xorbs = Vec::new();
    let mut chunks = Vec::new();
    for i in 0..4 {
        let body = labelled_xorb(&format!("x{i}"), 5, false);
        chunks.extend(stored(&body).chunks.hashes.iter().map(|(c, _)| *c));
        xorbs.push(upload_xorb(&server, body, &server.write_token).await);
    }
    let first = register_file(&server, "f", &xorbs, SCOPE_A, Visibility::Global).await;

    let answer = query(&server, &first, &server.read_token).await;
    let read = offered(&answer, &first);
    assert_eq!(read.xorbs, sorted(&xorbs));
    assert_eq!(read.files, 0);
    for chunk in &chunks {
        assert!(!reveals(&answer.body, chunk), "raw chunk hash {chunk} sent");
    }
    let file = chunk_hash(b"file/f");
    assert!(!answer.body.windows(32).any(|w| w == file.as_bytes()));
    assert_eq!(answer.headers[CACHE_CONTROL], "no-store");
}
