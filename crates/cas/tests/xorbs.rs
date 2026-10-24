//! `POST` and `HEAD /v1/xorbs/{prefix}/{hash}` (S10 §3; spec
//! <https://huggingface.co/docs/xet/api#3-upload-xorb> and
//! <https://huggingface.co/docs/xet/api#7-head-xorb>; design §5.3, §5.9).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;

use common::xorb::{
    HF_XET_XORB, SCHEME_NONE, chunk, chunk_data, hf_xet_xorb, raw_chunk, stored, xorb_of, xorb_path,
};
use common::{
    FailingBucket, OTHER_PREFIX_XORB_PATH, Reply, SCOPE_A, SCOPE_B, TestServer, bearer, config,
};
use openxet_index::Visibility;
use openxet_protocol::{
    MAX_XORB_BODY_BYTES, MAX_XORB_CHUNKS, MerkleHash, chunk_hash, hash_from_hex, hash_to_hex,
};
use openxet_storage::Key;
use reqwest::Method;
use reqwest::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH};

/// Uploads `body` as xorb `hash` with `token`, as `hf_xet` does: with a
/// `Content-Length` and no `Content-Type`.
async fn upload(server: &TestServer, hash: &MerkleHash, body: Vec<u8>, token: &str) -> Reply {
    Reply::of(
        server
            .request(Method::POST, &xorb_path(hash))
            .header(AUTHORIZATION, bearer(token))
            .body(body),
    )
    .await
}

async fn head(server: &TestServer, hash: &MerkleHash, token: &str) -> Reply {
    Reply::of(
        server
            .request(Method::HEAD, &xorb_path(hash))
            .header(AUTHORIZATION, bearer(token)),
    )
    .await
}

/// Asserts a `200 {"was_inserted": inserted}` answer.
fn assert_inserted(reply: &Reply, inserted: bool) {
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.headers["content-type"], "application/json",
        "{}",
        reply.body
    );
    assert_eq!(
        reply.json(),
        serde_json::json!({ "was_inserted": inserted })
    );
}

/// The stored bytes of xorb `hash`, read back from the bucket.
async fn bucket_bytes(server: &TestServer, hash: MerkleHash) -> Option<Vec<u8>> {
    let bucket = Arc::clone(&server.bucket);
    server
        .on_server(async move {
            let key = Key::Xorb(hash);
            let size = bucket.size(&key).await.unwrap()?;
            Some(bucket.get_range(&key, 0, size - 1).await.unwrap().to_vec())
        })
        .await
}

async fn index_size(server: &TestServer, hash: MerkleHash, scope: &'static str) -> Option<u64> {
    let index = Arc::clone(&server.index);
    server
        .on_server(async move {
            index
                .xorb_size(&hash, scope, Visibility::Global)
                .await
                .unwrap()
        })
        .await
}

#[tokio::test]
async fn upload_then_head() {
    let server = TestServer::start_default().await;
    let body = hf_xet_xorb();
    let hash = hash_from_hex(HF_XET_XORB).unwrap();
    let expected = stored(&body);

    assert_eq!(head(&server, &hash, &server.read_token).await.status, 404);
    let uploaded = upload(&server, &hash, body, &server.write_token).await;
    assert_inserted(&uploaded, true);
    // `was_inserted` is per scope (design §5.4).
    assert_eq!(uploaded.headers[CACHE_CONTROL], "no-store");

    let reply = head(&server, &hash, &server.read_token).await;
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.headers[CONTENT_LENGTH].to_str().unwrap(),
        expected.bytes.len().to_string()
    );
    assert!(reply.body.is_empty());
    let logs = server.logs();
    assert!(logs.contains(HF_XET_XORB), "the hash is not logged: {logs}");
    assert!(logs.contains("route=/v1/xorbs/{prefix}/{hash}"), "{logs}");
}

#[tokio::test]
async fn acknowledged_xorb_write_is_visible() {
    // Design §5.9 req 7: a 200 means the bytes are in the bucket and the
    // xorb is in the index, before the response is sent.
    let server = TestServer::start_default().await;
    let body = hf_xet_xorb();
    let hash = hash_from_hex(HF_XET_XORB).unwrap();
    let expected = stored(&body);

    assert_inserted(
        &upload(&server, &hash, body, &server.write_token).await,
        true,
    );

    assert_eq!(
        bucket_bytes(&server, hash).await,
        Some(expected.bytes.clone())
    );
    assert_eq!(
        index_size(&server, hash, SCOPE_A).await,
        Some(expected.bytes.len() as u64)
    );
    let index = Arc::clone(&server.index);
    let tables = server
        .on_server(async move {
            index
                .chunk_tables(
                    &[hash],
                    &openxet_auth::default_tenant(SCOPE_A),
                    Visibility::PerTenant,
                )
                .await
                .unwrap()
        })
        .await;
    let table = &tables[&hash];
    assert_eq!(&*table.hashes, expected.chunks.hashes.as_slice());
    assert_eq!(&*table.offsets, expected.chunk_offsets.as_slice());
}

#[tokio::test]
async fn xorb_upload_retry_after_commit_succeeds() {
    // hf_xet resends the whole body when a response is lost (S10 §3).
    let server = TestServer::start_default().await;
    let body = hf_xet_xorb();
    let hash = hash_from_hex(HF_XET_XORB).unwrap();

    assert_inserted(
        &upload(&server, &hash, body.clone(), &server.write_token).await,
        true,
    );
    let first = bucket_bytes(&server, hash).await.unwrap();
    assert_inserted(
        &upload(&server, &hash, body, &server.write_token).await,
        false,
    );

    assert_eq!(bucket_bytes(&server, hash).await.unwrap(), first);
    assert_eq!(
        server.bucket_files().len(),
        2,
        "the xorb and its owner record: {:?}",
        server.bucket_files()
    );
}

#[tokio::test]
async fn concurrent_identical_xorb_uploads() {
    let server = TestServer::start_default().await;
    let body = hf_xet_xorb();
    let hash = hash_from_hex(HF_XET_XORB).unwrap();

    let mut uploads = tokio::task::JoinSet::new();
    for _ in 0..8 {
        uploads.spawn(Reply::of(
            server
                .request(Method::POST, &xorb_path(&hash))
                .header(AUTHORIZATION, bearer(&server.write_token))
                .body(body.clone()),
        ));
    }
    let replies = uploads.join_all().await;

    let mut inserted = 0;
    for reply in &replies {
        assert_eq!(reply.status, 200, "{}", reply.body);
        if reply.json()["was_inserted"].as_bool().unwrap() {
            inserted += 1;
        }
    }
    assert_eq!(inserted, 1, "{replies:?}");
    assert_eq!(
        server.bucket_files().len(),
        2,
        "the xorb and its owner record: {:?}",
        server.bucket_files()
    );
    assert_eq!(bucket_bytes(&server, hash).await, Some(stored(&body).bytes));
}

#[tokio::test]
async fn consistent_footer_is_accepted() {
    // The native client sends no footer; a body that carries a consistent
    // one is accepted and stored the same (S10 §3).
    let server = TestServer::start_default().await;
    let footered = stored(&hf_xet_xorb()).bytes;
    let hash = hash_from_hex(HF_XET_XORB).unwrap();

    assert_inserted(
        &upload(&server, &hash, footered.clone(), &server.write_token).await,
        true,
    );

    assert_eq!(bucket_bytes(&server, hash).await, Some(footered));
}

#[tokio::test]
async fn hash_mismatch_is_400() {
    let server = TestServer::start_default().await;
    let body = xorb_of(&[b"some chunk data"]);
    let actual = stored(&body).hash;
    let claimed = hash_from_hex(HF_XET_XORB).unwrap();

    let reply = upload(&server, &claimed, body, &server.write_token).await;

    reply.assert_error(400, "xorb_hash_mismatch");
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains(HF_XET_XORB), "{message}");
    assert!(message.contains(&hash_to_hex(&actual)), "{message}");
    assert!(
        reply.json()["hint"]
            .as_str()
            .unwrap()
            .contains("content hash")
    );
    for hash in [claimed, actual] {
        assert_eq!(head(&server, &hash, &server.read_token).await.status, 404);
    }
    assert!(
        server.bucket_files().is_empty(),
        "{:?}",
        server.bucket_files()
    );
}

#[tokio::test]
async fn malformed_xorb_is_400_with_reason_code() {
    // One body per `ProtocolError` `Xorb*` variant `xorb_with_footer`
    // returns. `XorbTooLarge` can't be reached past the body limit; it is
    // `oversized_body_is_413`.
    let server = TestServer::start_default().await;
    let footered = stored(&xorb_of(&[b"abc"])).bytes;
    let too_many: Vec<u8> = (0..=MAX_XORB_CHUNKS).flat_map(|_| chunk(b"x")).collect();
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("xorb_empty", Vec::new()),
        ("xorb_too_many_chunks", too_many),
        ("xorb_chunk_empty", raw_chunk(SCHEME_NONE, b"", 0)),
        ("xorb_chunk_too_long", raw_chunk(SCHEME_NONE, &[7; 100], 50)),
        (
            "xorb_chunk_too_short",
            raw_chunk(SCHEME_NONE, &[7; 100], 200),
        ),
        ("xorb_truncated", {
            let mut body = raw_chunk(SCHEME_NONE, &[7; 100], 100);
            body.truncate(50);
            body
        }),
        ("xorb_trailing_bytes", [footered, vec![0]].concat()),
        // Scheme 7 is no compression scheme the spec defines.
        ("xorb_malformed", raw_chunk(7, &[7; 10], 10)),
    ];

    for (code, body) in cases {
        let hash = MerkleHash::default();
        upload(&server, &hash, body, &server.write_token)
            .await
            .assert_error(400, code);
    }
    assert!(
        server.bucket_files().is_empty(),
        "{:?}",
        server.bucket_files()
    );
}

#[tokio::test]
async fn oversized_body_is_413() {
    let server = TestServer::start_default().await;
    let limit = usize::try_from(MAX_XORB_BODY_BYTES).unwrap();

    // At the limit the body is read and validated: all-zero bytes are a
    // chunk header declaring an empty chunk.
    upload(
        &server,
        &MerkleHash::default(),
        vec![0; limit],
        &server.write_token,
    )
    .await
    .assert_error(400, "xorb_chunk_empty");

    // Over it, the declared length alone answers, before any byte is read.
    let head = format!(
        "POST {} HTTP/1.1\r\nAuthorization: {}\r\nContent-Length: {}",
        xorb_path(&MerkleHash::default()),
        bearer(&server.write_token),
        limit + 1
    );
    let answer = server.raw(&head, b"", false).await;
    assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");
    assert!(answer.contains("\"xorb_too_large\""), "{answer}");
    assert!(
        answer.to_ascii_lowercase().contains("connection: close"),
        "{answer}"
    );
}

#[tokio::test]
async fn read_token_cannot_upload_403() {
    let server = TestServer::start_default().await;
    let hash = hash_from_hex(HF_XET_XORB).unwrap();

    upload(&server, &hash, hf_xet_xorb(), &server.read_token)
        .await
        .assert_error(403, "insufficient_scope");

    assert_eq!(head(&server, &hash, &server.read_token).await.status, 404);
    assert!(
        server.bucket_files().is_empty(),
        "{:?}",
        server.bucket_files()
    );
}

#[tokio::test]
async fn storage_failure_registers_nothing() {
    // Design §5.3: bucket first, then index. A failed put must leave no
    // index row pointing at bytes the bucket lacks.
    let server = TestServer::start_with(config(Visibility::Global), Arc::new(FailingBucket)).await;
    let hash = hash_from_hex(HF_XET_XORB).unwrap();

    let reply = upload(&server, &hash, hf_xet_xorb(), &server.write_token).await;

    reply.assert_error(503, "unavailable");
    assert!(!reply.body.contains("failing-bucket"), "{}", reply.body);
    assert_eq!(head(&server, &hash, &server.read_token).await.status, 404);
    assert_eq!(index_size(&server, hash, SCOPE_A).await, None);
}

#[tokio::test]
async fn was_inserted_is_per_tenant() {
    // `was_inserted` never reveals that another tenant uploaded the xorb,
    // under either visibility (design §5.4).
    for visibility in [Visibility::Global, Visibility::PerTenant] {
        let server = TestServer::start(config(visibility)).await;
        let body = hf_xet_xorb();
        let hash = hash_from_hex(HF_XET_XORB).unwrap();

        assert_inserted(
            &upload(&server, &hash, body.clone(), &server.write_token).await,
            true,
        );
        assert_inserted(
            &upload(&server, &hash, body, &server.other_scope_token).await,
            true,
        );
        assert_eq!(
            server.bucket_files().len(),
            3,
            "the xorb, stored once, and each tenant's owner record"
        );
        assert_eq!(
            index_size(&server, hash, SCOPE_B).await,
            Some(stored(&hf_xet_xorb()).bytes.len() as u64)
        );
    }
}

#[tokio::test]
async fn head_under_per_tenant_hides_other_tenants_xorbs() {
    let hash = hash_from_hex(HF_XET_XORB).unwrap();
    for (visibility, other_status) in [(Visibility::Global, 200), (Visibility::PerTenant, 404)] {
        let server = TestServer::start(config(visibility)).await;
        assert_inserted(
            &upload(&server, &hash, hf_xet_xorb(), &server.write_token).await,
            true,
        );

        let other = head(&server, &hash, &server.other_scope_token).await;

        assert_eq!(other.status, other_status, "{visibility:?}");
        assert!(other.body.is_empty());
        let own = head(&server, &hash, &server.read_token).await;
        assert_eq!(own.status, 200);
        // Both answers depend on the caller's scope, so no cache may keep
        // either for another caller (design §5.9 req 3).
        for reply in [&other, &own] {
            assert_eq!(reply.headers[CACHE_CONTROL], "no-store", "{visibility:?}");
        }
    }
}

#[tokio::test]
async fn dedup_eligible_chunks_are_recorded() {
    let server = TestServer::start_default().await;
    let eligible = chunk_data(true);
    let ordinary = chunk_data(false);
    let body = xorb_of(&[&ordinary, &eligible]);
    let hash = stored(&body).hash;

    assert_inserted(
        &upload(&server, &hash, body, &server.write_token).await,
        true,
    );

    let index = Arc::clone(&server.index);
    let (eligible, ordinary) = (chunk_hash(&eligible), chunk_hash(&ordinary));
    let offered = server
        .on_server(async move {
            (
                index
                    .dedup_xorb(&eligible, SCOPE_A, Visibility::Global)
                    .await
                    .unwrap(),
                index
                    .dedup_xorb(&ordinary, SCOPE_A, Visibility::Global)
                    .await
                    .unwrap(),
            )
        })
        .await;
    assert_eq!(offered, (Some(hash), None));
}

#[tokio::test]
async fn invalid_path_is_refused() {
    let server = TestServer::start_default().await;
    let upper = HF_XET_XORB.to_ascii_uppercase();
    let short = &HF_XET_XORB[..63];

    for hash in [upper.as_str(), short, "not-a-hash"] {
        let path = format!("/v1/xorbs/default/{hash}");
        let reply = Reply::of(
            server
                .request(Method::POST, &path)
                .header(AUTHORIZATION, bearer(&server.write_token))
                .body(hf_xet_xorb()),
        )
        .await;
        reply.assert_error(400, "invalid_hash");
        assert!(!reply.body.contains(hash), "{}", reply.body);
        Reply::of(
            server
                .request(Method::HEAD, &path)
                .header(AUTHORIZATION, bearer(&server.read_token)),
        )
        .await
        .assert_error(400, "invalid_hash");
    }
    for method in [Method::POST, Method::HEAD] {
        Reply::of(
            server
                .request(method, OTHER_PREFIX_XORB_PATH)
                .header(AUTHORIZATION, bearer(&server.write_token)),
        )
        .await
        .assert_error(404, "not_found");
    }
    assert!(
        server.bucket_files().is_empty(),
        "{:?}",
        server.bucket_files()
    );
}

#[tokio::test]
async fn refusals_before_the_body_reach_the_client() {
    // A refusal sent with the body unread would reset the connection, and
    // the client would see a failed write, which it retries, instead of a
    // final 4xx (S10 "Status codes and retries"). 8 MiB outgrows the socket
    // buffers, so an unread body can't hide in them.
    let server = TestServer::start_default().await;
    let upper = format!("/v1/xorbs/default/{}", HF_XET_XORB.to_ascii_uppercase());
    let valid = xorb_path(&hash_from_hex(HF_XET_XORB).unwrap());
    let cases = [
        (
            upper.as_str(),
            server.write_token.as_str(),
            400,
            "invalid_hash",
        ),
        (
            OTHER_PREFIX_XORB_PATH,
            &server.write_token,
            404,
            "not_found",
        ),
        (&valid, &server.read_token, 403, "insufficient_scope"),
        (&valid, "oxt_test_unknown", 401, "invalid_credential"),
    ];

    for (path, token, status, code) in cases {
        for _ in 0..3 {
            Reply::of(
                server
                    .request(Method::POST, path)
                    .header(AUTHORIZATION, bearer(token))
                    .body(vec![0u8; 8 << 20]),
            )
            .await
            .assert_error(status, code);
        }
    }
    assert!(
        server.bucket_files().is_empty(),
        "{:?}",
        server.bucket_files()
    );
}

#[tokio::test]
async fn refusals_answer_expect_continue_before_the_body() {
    // A client that sends `Expect: 100-continue` waits for the server
    // before sending the body (RFC 9110 §10.1.1). A refusal is final, so it
    // answers at once, never `100 Continue`, and closes the connection
    // rather than read a body nobody wants. Without `Expect`, the body is
    // drained as before so the answer arrives intact.
    let server = TestServer::start_default().await;
    let upper = format!("/v1/xorbs/default/{}", HF_XET_XORB.to_ascii_uppercase());
    let valid = xorb_path(&hash_from_hex(HF_XET_XORB).unwrap());
    let body = vec![0u8; 1000];

    for (path, token, status) in [
        (upper.as_str(), server.write_token.as_str(), "400"),
        (&valid, &server.read_token, "403"),
        (&valid, "oxt_test_unknown", "401"),
    ] {
        let head = format!(
            "POST {path} HTTP/1.1\r\nAuthorization: {}\r\nContent-Length: {}",
            bearer(token),
            body.len()
        );

        let expecting = server
            .raw(&format!("{head}\r\nExpect: 100-continue"), b"", false)
            .await;
        assert!(
            expecting.starts_with(&format!("HTTP/1.1 {status}")),
            "{expecting}"
        );
        assert!(
            expecting.to_ascii_lowercase().contains("connection: close"),
            "{expecting}"
        );

        let plain = server.raw(&head, &body, false).await;
        assert!(plain.starts_with(&format!("HTTP/1.1 {status}")), "{plain}");
        assert!(!plain.contains("100 Continue"), "{plain}");
    }
}
