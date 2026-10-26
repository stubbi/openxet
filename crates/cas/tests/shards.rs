//! `POST /v1/shards` and `POST /v2/shards` (S10 §4; spec
//! <https://huggingface.co/docs/xet/api#4-upload-shard> and
//! <https://huggingface.co/docs/xet/api#5-streaming-shard-upload>; S6;
//! design §5.3, §5.9).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::shard::{CommitStage, Event, Line, hf_xet_files, hf_xet_shard, lines, terminal};
use common::xorb::{HF_XET_XORB, hf_xet_xorb, stored, xorb_path};
use common::{FailingBucket, Reply, SCOPE_A, SCOPE_B, SlowIndex, TestServer, bearer, config};
use openxet_index::{ChunkTable, FileRecord, Visibility};
use openxet_protocol::{MerkleHash, VerifyLimits, hash_from_hex, hash_to_hex, shard_hash};
use openxet_storage::Key;
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};

/// The message for a referenced xorb the caller can't see, identical
/// whether it is missing or another scope's (ruling M1-R17).
fn not_stored(xorb: &str) -> String {
    format!(
        "xorb {xorb} is not stored on this server. If the server was reset, clear the client's \
         Xet cache for this endpoint (~/.cache/huggingface/xet or $HF_XET_CACHE) and upload again."
    )
}

async fn upload_xorb(server: &TestServer, token: &str) {
    let hash = hash_from_hex(HF_XET_XORB).unwrap();
    let reply = Reply::of(
        server
            .request(Method::POST, &xorb_path(&hash))
            .header(AUTHORIZATION, bearer(token))
            .body(hf_xet_xorb()),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

/// `POST /v1/shards` with `body`, as `hf_xet` sends it: `Content-Length`,
/// no `Content-Type`.
async fn upload_v1(server: &TestServer, body: Vec<u8>, token: &str) -> Reply {
    Reply::of(
        server
            .request(Method::POST, "/v1/shards")
            .header(AUTHORIZATION, bearer(token))
            .body(body),
    )
    .await
}

/// `POST /v2/shards` with `body`, asserting a `200` NDJSON stream, and its
/// lines.
async fn upload_v2(server: &TestServer, body: Vec<u8>, token: &str) -> Vec<Line> {
    let reply = Reply::of(
        server
            .request(Method::POST, "/v2/shards")
            .header(AUTHORIZATION, bearer(token))
            .body(body),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.headers[CONTENT_TYPE], "application/x-ndjson");
    assert_eq!(reply.headers[CACHE_CONTROL], "no-cache");
    lines(&reply.body)
}

/// Asserts a v1 `200 {"result": result}`.
fn assert_result_v1(reply: &Reply, result: u8) {
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.headers[CONTENT_TYPE], "application/json");
    assert_eq!(reply.json(), serde_json::json!({ "result": result }));
}

/// Asserts a v2 stream that ends in `{"type":"result","result":result}`.
fn assert_result_v2(lines: &[Line], result: u8) {
    let last = terminal(lines);
    assert_eq!(last.event, Event::Result, "{lines:?}");
    assert_eq!(
        last.json,
        serde_json::json!({ "type": "result", "result": result })
    );
}

/// The terminal `error` event's message and `retryable`.
fn stream_error(lines: &[Line]) -> (String, bool) {
    match &terminal(lines).event {
        Event::Error { message, retryable } => {
            assert_eq!(
                terminal(lines).json["retryable"].as_bool(),
                Some(*retryable),
                "retryable must be sent explicitly"
            );
            (message.clone(), *retryable)
        }
        other => panic!("expected an error event, got {other:?} in {lines:?}"),
    }
}

async fn file(server: &TestServer, file: MerkleHash, scope: &'static str) -> Option<FileRecord> {
    let index = Arc::clone(&server.index);
    server
        .on_server(async move { index.file(&file, scope).await.unwrap() })
        .await
}

async fn files(server: &TestServer, scope: &'static str) -> Vec<Option<FileRecord>> {
    let mut out = Vec::new();
    for hash in hf_xet_files() {
        out.push(file(server, hash, scope).await);
    }
    out
}

/// The capture's stored shard, read back from the bucket. The capture is
/// already in the stored form, so it is stored byte for byte as sent.
async fn stored_shard(server: &TestServer) -> Option<Vec<u8>> {
    let bucket = Arc::clone(&server.bucket);
    let key = Key::Shard(shard_hash(&hf_xet_shard()));
    server
        .on_server(async move {
            let size = bucket.size(&key).await.unwrap()?;
            Some(bucket.get_range(&key, 0, size - 1).await.unwrap().to_vec())
        })
        .await
}

/// Registers the capture's xorb to `scope` in the index only, for servers
/// whose bucket can't store it.
async fn index_capture_xorb(server: &TestServer, scope: &'static str) {
    let xorb = stored(&hf_xet_xorb());
    let index = Arc::clone(&server.index);
    server
        .on_server(async move {
            let table = ChunkTable {
                hashes: xorb.chunks.hashes.into(),
                offsets: xorb.chunk_offsets.into(),
            };
            index
                .put_xorb(&xorb.hash, xorb.bytes.len() as u64, &table, &[], scope)
                .await
                .unwrap()
        })
        .await;
}

/// The capture with one byte of `a.bin`'s file hash flipped: it still
/// parses, but its file hash no longer matches its chunks.
fn tampered_shard() -> Vec<u8> {
    let mut body = hf_xet_shard();
    let target = hf_xet_files()[1];
    let at = body
        .windows(32)
        .position(|w| w == target.as_bytes())
        .expect("the capture holds a.bin's file hash");
    body[at] ^= 1;
    body
}

#[tokio::test]
async fn hf_xet_capture_registers_files() {
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;

    assert_result_v2(
        &upload_v2(&server, hf_xet_shard(), &server.write_token).await,
        1,
    );

    let xorb = hash_from_hex(HF_XET_XORB).unwrap();
    let registered = files(&server, SCOPE_A).await;
    let [empty, a, b] = registered.as_slice() else {
        unreachable!()
    };
    let empty = empty.as_ref().expect("the empty file is registered");
    assert_eq!((empty.size, empty.terms.len()), (0, 0));
    for record in [a, b] {
        let record = record.as_ref().expect("the file is registered");
        assert!(!record.terms.is_empty());
        assert!(record.terms.iter().all(|t| t.xorb == xorb));
        assert_eq!(
            record.size,
            record
                .terms
                .iter()
                .map(|t| u64::from(t.unpacked_bytes))
                .sum::<u64>()
        );
    }
    assert!(
        server.logs().contains("shard registered"),
        "{}",
        server.logs()
    );
}

#[tokio::test]
async fn unversioned_shards_route_answers_as_v1() {
    // git-xet 0.2.1 uploads its shard to `POST {cas}/shards`, with no
    // version, as huggingface.co's CAS accepts (spike S7,
    // compat/fixtures/lfs/hf/02-push-xet-lfs371--06-POST-shards.json).
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;

    let reply = Reply::of(
        server
            .request(Method::POST, "/shards")
            .header(AUTHORIZATION, bearer(&server.write_token))
            .body(hf_xet_shard()),
    )
    .await;

    assert_result_v1(&reply, 1);
    assert!(files(&server, SCOPE_A).await.iter().all(Option::is_some));
}

#[tokio::test]
async fn v2_stream_events_in_order_and_parse_as_upstream_types() {
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;

    let lines = upload_v2(&server, hf_xet_shard(), &server.write_token).await;

    let events: Vec<&Event> = lines.iter().map(|l| &l.event).collect();
    let validating = events
        .iter()
        .take_while(|e| matches!(e, Event::Validating { .. }))
        .count();
    assert!(validating >= 1, "{events:?}");
    assert_eq!(
        events[validating..],
        [
            &Event::Committing {
                stage: CommitStage::Uploading
            },
            &Event::Committing {
                stage: CommitStage::Syncing
            },
            &Event::Result,
        ],
        "{events:?}"
    );
    // Progress counts files, and ends with all three verified.
    assert_eq!(
        events[validating - 1],
        &Event::Validating {
            verified: 3,
            total: 3
        }
    );
    for line in &lines {
        assert!(!matches!(line.event, Event::Unknown), "{line:?}");
    }
    assert_result_v2(&lines, 1);
}

#[tokio::test]
async fn shard_with_missing_xorb_rejected() {
    // Design §5.9 req 5: a shard is verified before anything is
    // registered, and a referenced xorb must be stored.
    let server = TestServer::start_default().await;

    let lines = upload_v2(&server, hf_xet_shard(), &server.write_token).await;
    let (message, retryable) = stream_error(&lines);
    assert!(message.contains("not stored on this server"), "{message}");
    assert_eq!(message, not_stored(HF_XET_XORB));
    assert!(!retryable);

    let reply = upload_v1(&server, hf_xet_shard(), &server.write_token).await;
    reply.assert_error(400, "unknown_xorb");
    assert_eq!(reply.json()["message"], not_stored(HF_XET_XORB));

    assert_eq!(files(&server, SCOPE_A).await, [None, None, None]);
    assert_eq!(stored_shard(&server).await, None);
}

#[tokio::test]
async fn shard_verify_work_capped() {
    // The capture references more than one chunk and one xorb.
    for (limits, rule) in [
        (
            VerifyLimits {
                max_referenced_chunks: 1,
                ..VerifyLimits::default()
            },
            "chunks in total, over the limit of 1",
        ),
        (
            VerifyLimits {
                max_referenced_xorbs: 0,
                ..VerifyLimits::default()
            },
            "distinct xorbs, over the limit of 0",
        ),
    ] {
        let cfg = openxet_cas::CasConfig {
            verify_limits: limits,
            ..config(Visibility::Global)
        };
        let server = TestServer::start(cfg).await;
        upload_xorb(&server, &server.write_token).await;

        let (message, retryable) =
            stream_error(&upload_v2(&server, hf_xet_shard(), &server.write_token).await);
        assert!(message.contains(rule), "{message}");
        assert!(message.contains("split the upload"), "{message}");
        assert!(!retryable);

        let reply = upload_v1(&server, hf_xet_shard(), &server.write_token).await;
        reply.assert_error(400, "shard_verification_failed");
        assert!(
            reply.json()["message"].as_str().unwrap().contains(rule),
            "{}",
            reply.body
        );
        assert_eq!(files(&server, SCOPE_A).await, [None, None, None]);
        assert_eq!(stored_shard(&server).await, None);
    }
}

#[tokio::test]
async fn tampered_shard_rejected_in_stream_not_404() {
    // A status other than 200 on v2 would lose the message, and a 404 would
    // send the client to v1 with the same bad shard (S10 §4).
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;

    let (message, retryable) =
        stream_error(&upload_v2(&server, tampered_shard(), &server.write_token).await);
    assert!(message.contains("doesn't hash to"), "{message}");
    assert!(message.contains("rebuild the shard"), "{message}");
    assert!(message.contains("hashing#file-hashes"), "{message}");
    assert!(!retryable);

    let (message, retryable) =
        stream_error(&upload_v2(&server, b"not a shard".to_vec(), &server.write_token).await);
    assert!(message.contains("malformed shard"), "{message}");
    assert!(!retryable);

    upload_v1(&server, tampered_shard(), &server.write_token)
        .await
        .assert_error(400, "shard_verification_failed");
    upload_v1(&server, b"not a shard".to_vec(), &server.write_token)
        .await
        .assert_error(400, "shard_malformed");

    assert_eq!(files(&server, SCOPE_A).await, [None, None, None]);
    assert!(
        server
            .bucket_files()
            .iter()
            .all(|f| !f.to_string_lossy().contains("shard")),
        "{:?}",
        server.bucket_files()
    );
}

#[tokio::test]
async fn shard_upload_retry_after_commit_succeeds() {
    // hf_xet resends the whole shard when a response is lost (S10 §4).
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;

    assert_result_v2(
        &upload_v2(&server, hf_xet_shard(), &server.write_token).await,
        1,
    );
    let first = files(&server, SCOPE_A).await;
    let bucket = server.bucket_files();

    assert_result_v2(
        &upload_v2(&server, hf_xet_shard(), &server.write_token).await,
        0,
    );
    assert_result_v1(
        &upload_v1(&server, hf_xet_shard(), &server.write_token).await,
        0,
    );

    assert_eq!(files(&server, SCOPE_A).await, first);
    assert_eq!(server.bucket_files(), bucket);
    assert_eq!(
        bucket.len(),
        4,
        "one xorb, its owner record, one shard and its registration record: {bucket:?}"
    );
}

#[tokio::test]
async fn same_shard_other_scope_registers_again() {
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;

    assert_result_v1(
        &upload_v1(&server, hf_xet_shard(), &server.write_token).await,
        1,
    );
    // Global visibility: scope B dedups against scope A's xorb.
    assert_result_v1(
        &upload_v1(&server, hf_xet_shard(), &server.other_scope_token).await,
        1,
    );
    assert_result_v1(
        &upload_v1(&server, hf_xet_shard(), &server.other_scope_token).await,
        0,
    );

    assert_eq!(files(&server, SCOPE_B).await, files(&server, SCOPE_A).await);
    assert!(files(&server, SCOPE_B).await.iter().all(Option::is_some));
}

#[tokio::test]
async fn per_tenant_shard_cannot_reference_foreign_xorb() {
    // Ruling M1-R17: another scope's xorb answers exactly as a missing one,
    // so the error can't probe what other scopes stored.
    let missing = TestServer::start(config(Visibility::PerTenant)).await;
    let missing_v2 = stream_error(&upload_v2(&missing, hf_xet_shard(), &missing.write_token).await);
    let missing_v1 = upload_v1(&missing, hf_xet_shard(), &missing.write_token).await;

    let foreign = TestServer::start(config(Visibility::PerTenant)).await;
    upload_xorb(&foreign, &foreign.other_scope_token).await;
    let foreign_v2 = stream_error(&upload_v2(&foreign, hf_xet_shard(), &foreign.write_token).await);
    let foreign_v1 = upload_v1(&foreign, hf_xet_shard(), &foreign.write_token).await;

    assert_eq!(foreign_v2, missing_v2);
    assert_eq!(foreign_v2, (not_stored(HF_XET_XORB), false));
    foreign_v1.assert_error(400, "unknown_xorb");
    assert_eq!(foreign_v1.json(), missing_v1.json());
    assert_eq!(files(&foreign, SCOPE_A).await, [None, None, None]);

    // The owner registers it.
    assert_result_v2(
        &upload_v2(&foreign, hf_xet_shard(), &foreign.other_scope_token).await,
        1,
    );
    assert_eq!(files(&foreign, SCOPE_A).await, [None, None, None]);
}

#[tokio::test]
async fn shard_storage_failure_registers_nothing() {
    // Design §5.3: bucket first, then index. A failed put registers nothing,
    // and v2 tells the client to retry the whole upload (S10 §4).
    let server = TestServer::start_with(config(Visibility::Global), Arc::new(FailingBucket)).await;
    index_capture_xorb(&server, SCOPE_A).await;

    let lines = upload_v2(&server, hf_xet_shard(), &server.write_token).await;
    let (message, retryable) = stream_error(&lines);
    assert!(retryable, "{lines:?}");
    assert!(message.contains("storage"), "{message}");
    assert!(!message.contains("failing-bucket"), "{message}");
    assert!(
        lines.iter().any(|l| l.event
            == Event::Committing {
                stage: CommitStage::Uploading
            }),
        "{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.event
            == Event::Committing {
                stage: CommitStage::Syncing
            }),
        "{lines:?}"
    );
    let logs = server.logs();
    assert!(logs.contains("no space left on device"), "{logs}");

    upload_v1(&server, hf_xet_shard(), &server.write_token)
        .await
        .assert_error(503, "unavailable");

    assert_eq!(files(&server, SCOPE_A).await, [None, None, None]);
}

#[tokio::test]
async fn heartbeat_during_slow_verify() {
    // The client fails a stream silent for 300 s (S10 §4), so the last
    // progress event repeats while work is slow.
    let cfg = openxet_cas::CasConfig {
        heartbeat_interval: Duration::from_millis(50),
        ..config(Visibility::Global)
    };
    let server =
        TestServer::start_with_index(cfg, SlowIndex::wrap(Duration::from_millis(600))).await;
    upload_xorb(&server, &server.write_token).await;

    let lines = upload_v2(&server, hf_xet_shard(), &server.write_token).await;

    let repeats = lines
        .iter()
        .filter(|l| {
            l.event
                == Event::Validating {
                    verified: 0,
                    total: 3,
                }
        })
        .count();
    assert!(repeats >= 4, "{lines:?}");
    assert_result_v2(&lines, 1);
}

#[tokio::test]
async fn shard_commit_survives_client_disconnect() {
    // A client gone mid-stream doesn't abort the work: it runs to the end,
    // so the bucket is never written with the index left half-registered.
    let server = TestServer::start_with_index(
        config(Visibility::Global),
        SlowIndex::wrap(Duration::from_millis(500)),
    )
    .await;
    upload_xorb(&server, &server.write_token).await;

    let mut response = server
        .request(Method::POST, "/v2/shards")
        .header(AUTHORIZATION, bearer(&server.write_token))
        .body(hf_xet_shard())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(response.chunk().await.unwrap().is_some());
    drop(response);

    let registered = tokio::time::timeout(Duration::from_secs(10), async {
        while files(&server, SCOPE_A).await.iter().any(Option::is_none) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    registered
        .await
        .expect("the upload completes without its client");
    assert_eq!(stored_shard(&server).await, Some(hf_xet_shard()));
}

#[tokio::test]
async fn acknowledged_shard_write_is_visible() {
    // Design §5.9 req 7: a result means the shard is in the bucket and its
    // files are in the index, before the answer is sent.
    for v2 in [false, true] {
        let server = TestServer::start_default().await;
        upload_xorb(&server, &server.write_token).await;

        if v2 {
            assert_result_v2(
                &upload_v2(&server, hf_xet_shard(), &server.write_token).await,
                1,
            );
        } else {
            assert_result_v1(
                &upload_v1(&server, hf_xet_shard(), &server.write_token).await,
                1,
            );
        }

        assert_eq!(stored_shard(&server).await, Some(hf_xet_shard()));
        assert!(files(&server, SCOPE_A).await.iter().all(Option::is_some));
    }
}

#[tokio::test]
async fn refused_shard_uploads_answer_with_a_status() {
    // Authorization and size come before the stream: a 401, 403 or 413 is
    // final on both versions (S10 §4), and each refused body is drained or
    // the connection closed (ruling M1-R22).
    let server = TestServer::start_default().await;
    for path in ["/v1/shards", "/v2/shards"] {
        Reply::of(
            server
                .request(Method::POST, path)
                .header(AUTHORIZATION, bearer(&server.read_token))
                .body(vec![0u8; 8 << 20]),
        )
        .await
        .assert_error(403, "insufficient_scope");

        let head = format!(
            "POST {path} HTTP/1.1\r\nAuthorization: {}\r\nContent-Length: {}",
            bearer(&server.write_token),
            openxet_protocol::MAX_UPLOAD_SHARD_BYTES + 1
        );
        let answer = server.raw(&head, b"", false).await;
        assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");
        assert!(answer.contains("\"shard_too_large\""), "{answer}");
    }
}

/// Every run of 64 lowercase hex characters in `text`.
fn hexes(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let run = bytes[i..]
            .iter()
            .take_while(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
            .count();
        if run == 64 {
            out.push(text[i..i + 64].to_owned());
        }
        i += run.max(1);
    }
    out
}

fn find(body: &[u8], hash: &MerkleHash) -> Option<usize> {
    body.windows(32).position(|w| w == hash.as_bytes())
}

/// What the server derived from the capture's stored chunks: the true
/// verification and file hashes, as hex, and the segment lengths.
fn capture_secrets() -> (Vec<String>, Vec<u32>) {
    let shard = openxet_protocol::read_upload_shard(hf_xet_shard().as_slice()).unwrap();
    let mut hashes = Vec::new();
    let mut lengths = Vec::new();
    for file in shard.files.iter().filter(|f| !f.segments.is_empty()) {
        hashes.push(hash_to_hex(&file.file_hash));
        for seg in &file.segments {
            hashes.push(hash_to_hex(&seg.verification.unwrap()));
            lengths.push(seg.unpacked_bytes);
        }
    }
    (hashes, lengths)
}

fn assert_reveals_nothing(text: &str, secrets: &(Vec<String>, Vec<u32>), chunks: usize) {
    for hash in &secrets.0 {
        assert!(!text.contains(hash.as_str()), "leaks {hash}: {text}");
    }
    for length in &secrets.1 {
        assert!(
            !text.contains(&length.to_string()),
            "leaks {length}: {text}"
        );
    }
    assert!(
        !text.contains(&format!("{chunks} chunks")),
        "leaks the chunk count: {text}"
    );
}

#[tokio::test]
async fn foreign_writer_cannot_forge_shard_under_global() {
    // Ruling M1-R23. Verification proves the uploader holds the chunks it
    // references. Under global visibility a writer of another scope may
    // learn a xorb's hash; if errors echoed the values the server derives
    // from its stored chunks, it could patch a shard from them until it
    // verifies, and register files over data it never had.
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;
    let xorb = hash_from_hex(HF_XET_XORB).unwrap();
    let chunks = stored(&hf_xet_xorb()).chunks.hashes.len();
    let secrets = capture_secrets();

    // The attacker's shard: the capture's layout, with every verification
    // and file hash replaced by a guess.
    let mut body = hf_xet_shard();
    for hash in &secrets.0 {
        let at = find(&body, &hash_from_hex(hash).unwrap()).unwrap();
        body[at] ^= 0xff;
    }
    // From each message, the attacker takes any hash it didn't send and
    // writes it over a hash it did.
    for _ in 0..10 {
        let reply = upload_v1(&server, body.clone(), &server.other_scope_token).await;
        assert_ne!(reply.status, 200, "forged shard accepted: {}", reply.body);
        let message = reply.json()["message"].as_str().unwrap().to_owned();
        let (message_v2, _) =
            stream_error(&upload_v2(&server, body.clone(), &server.other_scope_token).await);
        assert_reveals_nothing(&message, &secrets, chunks);
        assert_reveals_nothing(&message_v2, &secrets, chunks);
        let seen: Vec<MerkleHash> = hexes(&message)
            .iter()
            .map(|h| hash_from_hex(h).unwrap())
            .collect();
        let revealed = seen.iter().find(|h| find(&body, h).is_none());
        let sent = seen
            .iter()
            .find(|h| **h != xorb && find(&body, h).is_some());
        if let (Some(revealed), Some(sent)) = (revealed, sent) {
            let at = find(&body, sent).unwrap();
            body[at..at + 32].copy_from_slice(revealed.as_bytes());
        }
    }
    assert_eq!(files(&server, SCOPE_B).await, [None, None, None]);

    // The length and bounds rules don't reveal the stored values either.
    let original = hf_xet_shard();
    // The first segment entry follows the empty file: xorb hash, flags,
    // unpacked bytes, chunk start, chunk end, each a u32 LE
    // (https://huggingface.co/docs/xet/shard#filedatasequenceentry).
    let at = find(&original, &xorb).unwrap();
    assert_eq!(
        u32::from_le_bytes(original[at + 36..at + 40].try_into().unwrap()),
        secrets.1[0]
    );
    let mut length = original.clone();
    length[at + 36..at + 40].copy_from_slice(&1u32.to_le_bytes());
    let mut bounds = original.clone();
    let past_end = u32::try_from(chunks + 1).unwrap();
    bounds[at + 44..at + 48].copy_from_slice(&past_end.to_le_bytes());
    for (body, rule) in [(length, "unpacked bytes"), (bounds, "out of bounds")] {
        let reply = upload_v1(&server, body.clone(), &server.other_scope_token).await;
        reply.assert_error(400, "shard_verification_failed");
        let message = reply.json()["message"].as_str().unwrap().to_owned();
        assert!(message.contains(rule), "{message}");
        assert_reveals_nothing(&message, &secrets, chunks);
        let (message_v2, _) =
            stream_error(&upload_v2(&server, body, &server.other_scope_token).await);
        assert_reveals_nothing(&message_v2, &secrets, chunks);
    }
    assert_eq!(files(&server, SCOPE_B).await, [None, None, None]);
}

#[tokio::test]
async fn rejections_are_logged_with_their_full_reason() {
    // The v1 client never shows the body, so the operator's log holds the
    // reason, server-derived values included, under the request id.
    let server = TestServer::start_default().await;
    upload_xorb(&server, &server.write_token).await;

    let reply = upload_v1(&server, tampered_shard(), &server.write_token).await;
    reply.assert_error(400, "shard_verification_failed");
    let id = reply.headers["x-request-id"].to_str().unwrap().to_owned();
    let true_file_hash = common::shard::HF_XET_FILES[1];
    assert!(!reply.body.contains(true_file_hash), "{}", reply.body);
    let logs = server.logs();
    let line = logs
        .lines()
        .find(|l| l.contains(&id) && l.contains("WARN") && l.contains(true_file_hash))
        .unwrap_or_else(|| panic!("no WARN line with the full reason: {logs}"));
    assert!(line.contains("shard_verification_failed"), "{line}");

    let reply = upload_v1(&server, b"not a shard".to_vec(), &server.write_token).await;
    let id = reply.headers["x-request-id"].to_str().unwrap().to_owned();
    let logs = server.logs();
    assert!(
        logs.lines()
            .any(|l| l.contains(&id) && l.contains("WARN") && l.contains("shard_malformed")),
        "{logs}"
    );
}

#[tokio::test]
async fn stored_shard_is_the_servers_reserialization() {
    // The bucket keeps a shard rebuilt from verified file info and the
    // server's chunk tables, never the raw body (S6 "What gets stored"). The
    // reference shard carries lookup tables and a footer, which the
    // stored form drops.
    const XORB: &str = "eea25d6ee393ccae385820daed127b96ef0ea034dfb7cf6da3a950ce334b7632";
    let reference = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/xet-spec-reference");
    let read = |name: &str| {
        std::fs::read(reference.join(name)).unwrap_or_else(|e| {
            panic!("missing fixture {name} ({e}): run scripts/fetch-reference-files.sh")
        })
    };
    // The reference xorb file ends in 4 stray bytes (`XETB`).
    let mut xorb = read(&format!("{XORB}.xorb"));
    xorb.truncate(xorb.len() - 4);
    let body = read("Electric_Vehicle_Population_Data_20250917.csv.shard.verification");
    let server = TestServer::start_default().await;
    let reply = Reply::of(
        server
            .request(Method::POST, &xorb_path(&hash_from_hex(XORB).unwrap()))
            .header(AUTHORIZATION, bearer(&server.write_token))
            .body(xorb.clone()),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);

    assert_result_v1(
        &upload_v1(&server, body.clone(), &server.write_token).await,
        1,
    );

    let stored_xorb = stored(&xorb);
    let chunks: Arc<[(MerkleHash, u64)]> = stored_xorb.chunks.hashes.into();
    let lookup = move |x: &MerkleHash| (*x == stored_xorb.hash).then(|| Arc::clone(&chunks));
    let shard = openxet_protocol::read_upload_shard(body.as_slice()).unwrap();
    let expected = openxet_protocol::stored_shard(&shard, &lookup).unwrap();
    assert_ne!(expected, body);
    let bucket = Arc::clone(&server.bucket);
    let key = Key::Shard(shard_hash(&expected));
    let in_bucket = server
        .on_server(async move {
            let size = bucket.size(&key).await.unwrap()?;
            Some(bucket.get_range(&key, 0, size - 1).await.unwrap().to_vec())
        })
        .await;
    assert_eq!(in_bucket, Some(expected));
}
