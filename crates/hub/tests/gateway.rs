//! The HTTP file gateway, `GET|HEAD /v1/transfer/files/{repo}/{file}`
//! (ruling M2-R9): spike S4's conformance list
//! (`docs/research/S4-analytics-clients.md`), byte-exact reads of the
//! reference file and of a file over three xorbs, and design §5.9's
//! requirements 1 and 2 for the Hub's URLs.

#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::http::header::{IF_RANGE, LINK, LOCATION, RANGE};
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use openxet_auth::RulesAuthorizer;
use openxet_hub::test_support::{BEARER, PUBLIC_URL, Reply, TestHub};
use openxet_index::{ChunkTable, FileRecord, NewFile, Registrant, Term, Visibility};
use openxet_protocol::{chunk_hash, file_hash, hash_to_hex, xorb_with_footer};
use openxet_storage::{Key, LocalBucket};
use sha2::Digest as _;
use support::{
    A_BIN_HASH, A_BIN_SHA256, ALICE, BOB, CAROL, commit_ok, file, header, hub, lfs_file, repo_id,
    send, uploaded, xet_token,
};
use tower::ServiceExt as _;

/// The reference file of `xet-team/xet-spec-reference-files`
/// (`scripts/fetch-reference-files.sh`).
const REFERENCE: &str = "Electric_Vehicle_Population_Data_20250917.csv";

/// The empty file of the `hf_xet` 1.7.0 capture.
const EMPTY_FILE: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn reference(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/xet-spec-reference")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; run scripts/fetch-reference-files.sh",
            path.display()
        )
    })
}

/// Uploads the reference file's xorb and shard to `repo` through the CAS
/// with a write token from the Hub; returns its Xet hash.
async fn upload_reference(hub: &TestHub, router: &Router, repo: &str) -> String {
    let token = xet_token(hub, ALICE, repo, "write").await;
    let xorb_hash = String::from_utf8(reference(&format!("{REFERENCE}.xet-xorb-hash"))).unwrap();
    let mut xorb = reference(&format!("{}.xorb", xorb_hash.trim()));
    // The reference xorb file ends in 4 stray bytes (`XETB`).
    xorb.truncate(xorb.len() - 4);
    let reply = send(
        router,
        Method::POST,
        &format!("/v1/xorbs/default/{}", xorb_hash.trim()),
        &token,
        xorb,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "xorb: {}", reply.text());
    let shard = reference(&format!("{REFERENCE}.shard.verification"));
    let reply = send(router, Method::POST, "/v1/shards", &token, shard).await;
    assert_eq!(reply.status, StatusCode::OK, "shard: {}", reply.text());
    String::from_utf8(reference(&format!("{REFERENCE}.xet-file-hash")))
        .unwrap()
        .trim()
        .to_owned()
}

/// A file stored as the CAS stores one: its xorbs in the bucket and their
/// chunk layouts and the file's terms in the index, registered to
/// `repo`'s scope with its SHA-256.
struct Stored {
    hash: String,
    sha256: String,
    content: Vec<u8>,
    /// Where each term's chunks start in the file, and where it ends.
    boundaries: Vec<u64>,
}

/// One uncompressed chunk holding `data`, framed per the spec's chunk
/// header (<https://huggingface.co/docs/xet/xorb#chunk-header-structure>).
fn chunk(data: &[u8]) -> Vec<u8> {
    let len = u32::try_from(data.len()).unwrap().to_le_bytes();
    let mut out = vec![0, len[0], len[1], len[2], 0, len[0], len[1], len[2]];
    out.extend_from_slice(data);
    out
}

/// Stores `xorbs` (each a list of chunk contents) and registers to `repo`
/// the file whose terms are `terms`, `(xorb, first chunk, end chunk)`.
async fn store_file(
    hub: &TestHub,
    repo: &str,
    xorbs: &[Vec<Vec<u8>>],
    terms: &[(usize, u32, u32)],
) -> Stored {
    index_file(hub, repo, xorbs, terms, true).await
}

/// [`store_file`], leaving the xorbs out of the bucket unless `in_bucket`.
async fn index_file(
    hub: &TestHub,
    repo: &str,
    xorbs: &[Vec<Vec<u8>>],
    terms: &[(usize, u32, u32)],
    in_bucket: bool,
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
        if in_bucket {
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
        sha256: Some(sha256),
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
fn data(seed: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| {
            seed.wrapping_mul(31)
                .wrapping_add(u8::try_from(i % 251).unwrap())
        })
        .collect()
}

/// A file over three xorbs whose terms revisit the first xorb, so its
/// plan has fetches of one xorb apart: chunks of 1,000 to 7,000 bytes.
async fn three_xorb_file(hub: &TestHub, repo: &str) -> Stored {
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
async fn request(router: &Router, method: Method, path: &str, headers: &[(&str, String)]) -> Reply {
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

fn bearer(token: &str) -> (&'static str, String) {
    ("authorization", format!("{BEARER} {token}"))
}

/// S4's G1, G2 and G8 on `url`: `HEAD` answers every header a `GET` does,
/// without a body, and ignores `Range`; `GET` answers the whole file, with
/// the same `ETag`.
async fn assert_whole(router: &Router, url: &str, content: &[u8], etag: &str) {
    let size = content.len() as u64;
    for extra in [None, Some((RANGE.as_str(), "bytes=0-0".to_owned()))] {
        let mut headers = vec![bearer(ALICE)];
        headers.extend(extra);
        let head = request(router, Method::HEAD, url, &headers).await;
        assert_eq!(head.status, StatusCode::OK, "{url}: {}", head.text());
        assert!(head.body.is_empty());
        assert_eq!(
            head.header("content-length"),
            Some(size.to_string().as_str())
        );
        assert_eq!(head.header("accept-ranges"), Some("bytes"));
        assert_eq!(head.header("etag"), Some(etag));
        assert_eq!(
            head.header("content-type"),
            Some("application/octet-stream")
        );
        assert!(head.header("content-disposition").is_some());
    }
    // G2: the whole file.
    let whole = request(router, Method::GET, url, &[bearer(ALICE)]).await;
    assert_eq!(whole.status, StatusCode::OK);
    assert_eq!(
        whole.header("content-length"),
        Some(size.to_string().as_str())
    );
    assert!(whole.body == content, "{url}: the whole file differs");
    // G8: the same ETag on every answer.
    assert_eq!(whole.header("etag"), Some(etag));
}

/// The longest range [`assert_conforms`] reads beyond the whole file: a
/// debug build decodes the reference file's 63 MB in about a second.
const LONGEST_RANGE: u64 = 1 << 20;

/// Checks S4's list (G1–G9) on the gateway URL `url` of a file holding
/// `content`, whose `ETag` is `etag`, at the byte offsets in `edges` and
/// next to them. Of a file larger than [`LONGEST_RANGE`] it reads ranges
/// up to that long, and the whole file once.
async fn assert_conforms(router: &Router, url: &str, content: &[u8], etag: &str, edges: &[u64]) {
    let size = content.len() as u64;
    let small = size <= LONGEST_RANGE;
    let alice = bearer(ALICE);
    assert_whole(router, url, content, etag).await;

    let ranged = |range: String, if_range: Option<&str>| {
        let mut headers = vec![alice.clone(), (RANGE.as_str(), range)];
        if let Some(v) = if_range {
            headers.push((IF_RANGE.as_str(), v.to_owned()));
        }
        async move { request(router, Method::GET, url, &headers).await }
    };
    let assert_part = |reply: &Reply, start: u64, end: u64, what: &str| {
        assert_eq!(
            reply.status,
            StatusCode::PARTIAL_CONTENT,
            "{what}: {}",
            reply.text()
        );
        assert_eq!(
            reply.header("content-range"),
            Some(format!("bytes {start}-{end}/{size}").as_str()),
            "{what}"
        );
        assert_eq!(
            reply.header("content-length"),
            Some((end - start + 1).to_string().as_str()),
            "{what}"
        );
        assert_eq!(reply.header("etag"), Some(etag), "{what}");
        assert!(
            reply.body[..]
                == content[usize::try_from(start).unwrap()..=usize::try_from(end).unwrap()],
            "{what}: the bytes differ"
        );
    };
    // G3: `a-b` around every edge, across chunk, term and xorb boundaries.
    let mut points: Vec<u64> = edges
        .iter()
        .flat_map(|&e| [e.saturating_sub(1), e, e + 1])
        .filter(|&p| p < size)
        .collect();
    points.extend([0, size - 1]);
    points.sort_unstable();
    points.dedup();
    for (i, &start) in points.iter().enumerate() {
        for &end in points[i..].iter().filter(|&&e| e - start < LONGEST_RANGE) {
            let reply = ranged(format!("bytes={start}-{end}"), None).await;
            assert_part(&reply, start, end, &format!("bytes={start}-{end}"));
        }
    }
    // G4, G5, G7: open-ended, suffix and clamped ranges.
    for &start in points.iter().filter(|&&p| size - p <= LONGEST_RANGE) {
        let reply = ranged(format!("bytes={start}-"), None).await;
        assert_part(&reply, start, size - 1, &format!("bytes={start}-"));
        let reply = ranged(format!("bytes={start}-{}", size + 1000), None).await;
        assert_part(&reply, start, size - 1, "clamped");
        let n = size - start;
        let reply = ranged(format!("bytes=-{n}"), None).await;
        assert_part(&reply, start, size - 1, &format!("bytes=-{n}"));
    }
    if small {
        let reply = ranged(format!("bytes=-{}", size + 5), None).await;
        assert_part(&reply, 0, size - 1, "a suffix longer than the file");
    }
    // G6: past the end, and an empty suffix.
    for range in [
        format!("bytes={size}-"),
        format!("bytes={}-{}", size + 5, size + 10),
        "bytes=-0".to_owned(),
    ] {
        let reply = ranged(range.clone(), None).await;
        assert_eq!(reply.status, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
        assert_eq!(
            reply.header("content-range"),
            Some(format!("bytes */{size}").as_str()),
            "{range}"
        );
    }
    // G9: If-Range with the current ETag keeps the range; with another, or
    // a date, the answer is the whole file.
    let reply = ranged("bytes=1-2".to_owned(), Some(etag)).await;
    assert_part(&reply, 1, 2, "If-Range match");
    if !small {
        return;
    }
    for other in [
        "\"0000\"",
        "Wed, 21 Oct 2015 07:28:00 GMT",
        &format!("W/{etag}"),
    ] {
        let reply = ranged("bytes=1-2".to_owned(), Some(other)).await;
        assert_eq!(reply.status, StatusCode::OK, "If-Range {other}");
        assert!(reply.body == content, "If-Range {other}");
    }
    // A Range this server doesn't serve is ignored (RFC 9110 §14.2).
    for range in ["bytes=5-2", "bytes=0-1,4-5", "items=0-1", "bytes=x-"] {
        let reply = ranged(range.to_owned(), None).await;
        assert_eq!(reply.status, StatusCode::OK, "{range}");
        assert!(reply.body == content, "{range}");
    }
}

#[tokio::test]
async fn gateway_conformance() {
    let hub = hub().await;
    let (router, commit) = uploaded(&hub).await;
    let id = repo_id(&hub, "alice", "m").await;

    // A file over three xorbs, byte-exact at every term boundary and next
    // to it, which are chunk and xorb boundaries too.
    let three = three_xorb_file(&hub, &id).await;
    let url = format!("/v1/transfer/files/{id}/{}?name=three.bin", three.hash);
    let etag = format!("\"{}\"", three.sha256);
    assert_conforms(&router, &url, &three.content, &etag, &three.boundaries).await;
    let head = request(&router, Method::HEAD, &url, &[bearer(ALICE)]).await;
    assert_eq!(
        head.header("content-disposition"),
        Some("inline; filename*=UTF-8''three.bin; filename=\"three.bin\";")
    );
    // Without `commit`, no Hub metadata: the gateway can't know a revision.
    assert_eq!(head.header("x-xet-hash"), Some(three.hash.as_str()));
    assert_eq!(head.header("x-repo-commit"), None);
    assert_eq!(head.header("link"), None);

    // The reference file, uploaded as stock clients upload it; its shard
    // names its SHA-256, the ETag.
    let reference_hash = upload_reference(&hub, &router, "alice/m").await;
    let csv = reference(REFERENCE);
    let url = format!("/v1/transfer/files/{id}/{reference_hash}");
    let etag = format!("\"{}\"", hex(&sha2::Sha256::digest(&csv)));
    let edges = [1 << 16, 1 << 20, 30_000_000, csv.len() as u64 - 100];
    assert_conforms(&router, &url, &csv, &etag, &edges).await;
    let head = request(&router, Method::HEAD, &url, &[bearer(ALICE)]).await;
    assert_eq!(
        head.header("content-disposition"),
        Some(
            format!("inline; filename*=UTF-8''{reference_hash}; filename=\"{reference_hash}\";")
                .as_str()
        )
    );

    // The capture's `a.bin`, through resolve's own Location.
    let resolved = send(
        &router,
        Method::HEAD,
        "/alice/m/resolve/main/big.bin",
        ALICE,
        Vec::new(),
    )
    .await;
    let location = resolved.header("location").unwrap().to_owned();
    let path = location.strip_prefix(PUBLIC_URL).unwrap();
    let get = send(&router, Method::GET, path, ALICE, Vec::new()).await;
    assert_eq!(get.status, StatusCode::OK);
    assert_eq!(hex(&sha2::Sha256::digest(&get.body)), A_BIN_SHA256);
    assert_eq!(get.header("x-repo-commit"), Some(commit.as_str()));
    assert_eq!(get.header("x-xet-hash"), Some(A_BIN_HASH));
    assert_eq!(get.header("link"), resolved.header("link"));

    // The empty file: an empty body, and no range of it exists.
    let url = format!("/v1/transfer/files/{id}/{EMPTY_FILE}");
    let empty = request(&router, Method::GET, &url, &[bearer(ALICE)]).await;
    assert_eq!(empty.status, StatusCode::OK);
    assert_eq!(empty.header("content-length"), Some("0"));
    assert!(empty.body.is_empty());
    let range = request(
        &router,
        Method::GET,
        &url,
        &[bearer(ALICE), (RANGE.as_str(), "bytes=0-".to_owned())],
    )
    .await;
    assert_eq!(range.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(range.header("content-range"), Some("bytes */0"));

    // A commit that isn't a commit id is refused, not echoed.
    let bad = format!("/v1/transfer/files/{id}/{A_BIN_HASH}?commit=main%0d%0aX-Evil:%201");
    let reply = request(&router, Method::HEAD, &bad, &[bearer(ALICE)]).await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(reply.header("x-evil").is_none());
}

#[tokio::test]
async fn gateway_missing_xorb_is_a_server_fault() {
    // The index names a xorb the bucket lacks: an integrity fault, a `500`
    // before any byte, never a short body that reads as the file.
    let hub = hub().await;
    let (router, _) = uploaded(&hub).await;
    let id = repo_id(&hub, "alice", "m").await;
    let lost = index_file(
        &hub,
        &id,
        &[vec![data(1, 3000), data(2, 3000)]],
        &[(0, 0, 2)],
        false,
    )
    .await;
    let url = format!("/v1/transfer/files/{id}/{}", lost.hash);
    let reply = request(&router, Method::GET, &url, &[bearer(ALICE)]).await;
    assert_eq!(
        reply.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        reply.text()
    );
    assert_eq!(reply.json()["code"], "internal");
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        write!(out, "{b:02x}").unwrap();
        out
    })
}

#[tokio::test]
async fn gateway_needs_credential_scoped_to_repo() {
    // Design §5.9 req 2: the gateway authorizes the caller's own
    // credential for the repository the URL names; any other answers
    // exactly as a repository that doesn't exist (ruling M2-R6).
    let hub = hub().await;
    let (router, _) = uploaded(&hub).await;
    hub.create(CAROL, "carol", "c", None).await;
    let alice_repo = repo_id(&hub, "alice", "m").await;
    let carol_repo = repo_id(&hub, "carol", "c").await;
    let url = |repo: &str| format!("/v1/transfer/files/{repo}/{A_BIN_HASH}?name=big.bin");
    let gone = "0190f0f0f0f0700080000000000000ff";
    let get = |path: String, auth: Option<String>| {
        let router = router.clone();
        async move {
            let headers: Vec<(&str, String)> =
                auth.map(|a| ("authorization", a)).into_iter().collect();
            request(&router, Method::GET, &path, &headers).await
        }
    };
    let bearer = |t: &str| Some(format!("{BEARER} {t}"));
    let basic = |t: &str| {
        let pair = base64::engine::general_purpose::STANDARD.encode(format!("git:{t}"));
        Some(format!("Basic {pair}"))
    };

    // Alice and Bob may read alice/m, with `Bearer` or `Basic` (ruling M2-R9).
    for auth in [bearer(ALICE), basic(ALICE), bearer(BOB), basic(BOB)] {
        let reply = get(url(&alice_repo), auth).await;
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(hex(&sha2::Sha256::digest(&reply.body)), A_BIN_SHA256);
    }
    // Carol may not: her token gets the unknown-repository answer, exactly
    // as a repository that doesn't exist or an id that can't be one.
    let unknown = get(url(&alice_repo), bearer(CAROL)).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.header("x-error-code"), Some("RepoNotFound"));
    for other in [url(gone), url("not-an-id")] {
        assert_eq!(
            get(other, bearer(CAROL)).await.comparable(),
            unknown.comparable()
        );
    }
    assert_eq!(
        get(url(&alice_repo), basic(CAROL)).await.comparable(),
        unknown.comparable()
    );
    // Her own repository's scope never registered the file: it reads as
    // never stored there.
    let foreign = get(url(&carol_repo), bearer(CAROL)).await;
    assert_eq!(foreign.status, StatusCode::NOT_FOUND);
    assert_eq!(foreign.header("x-error-code"), Some("EntryNotFound"));
    // A CAS token, even alice/m's own, is not a user credential.
    let cas = xet_token(&hub, ALICE, "alice/m", "read").await;
    assert_eq!(
        get(url(&alice_repo), bearer(&cas)).await.status,
        StatusCode::FORBIDDEN
    );
    // Without a credential, or with a bad one: 401, the same whatever
    // exists.
    let anonymous = get(url(&alice_repo), None).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert!(anonymous.header("www-authenticate").is_some());
    assert_eq!(
        get(url(gone), None).await.comparable(),
        anonymous.comparable()
    );
    for bad in [
        bearer("oxt_test_never_configured"),
        basic("oxt_test_never_configured"),
        Some("Basic not-base64".to_owned()),
        Some("Digest x".to_owned()),
    ] {
        let reply = get(url(&alice_repo), bad.clone()).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{bad:?}");
        assert_eq!(
            reply.header("x-error-message"),
            Some("Invalid credentials in Authorization header")
        );
    }
}

/// Every URL in `reply`'s `Location` and `Link` headers.
fn header_urls(reply: &Reply) -> Vec<String> {
    let mut urls: Vec<String> = reply
        .header(LOCATION.as_str())
        .map(str::to_owned)
        .into_iter()
        .collect();
    if let Some(link) = reply.header(LINK.as_str()) {
        urls.extend(link.split(',').filter_map(|part| {
            let (_, rest) = part.split_once('<')?;
            rest.split_once('>').map(|(url, _)| url.to_owned())
        }));
    }
    urls
}

#[tokio::test]
async fn no_unauthenticated_reads() {
    // Design §5.9 req 1, the Hub's half: every URL the Hub answers with,
    // requested without a credential, is refused. LFS hrefs join with the
    // LFS routes (Task 14).
    let hub = hub().await;
    let (router, commit) = uploaded(&hub).await;
    let id = repo_id(&hub, "alice", "m").await;
    let three = three_xorb_file(&hub, &id).await;
    let lines = [
        header("three"),
        lfs_file("three.bin", &three.sha256, three.content.len() as u64),
    ];
    commit_ok(&hub, "alice/m", "main", &lines).await;

    let mut urls = Vec::new();
    for path in [
        "/alice/m/resolve/main/big.bin",
        "/alice/m/resolve/main/three.bin",
        &format!("/alice/m/resolve/{commit}/big.bin"),
        "/api/models/alice/m/tree/main?limit=1",
    ] {
        for method in [Method::HEAD, Method::GET] {
            let reply = send(&router, method.clone(), path, ALICE, Vec::new()).await;
            assert!(
                reply.status.is_success() || reply.status.is_redirection(),
                "{method} {path}: {}",
                reply.text()
            );
            urls.extend(header_urls(&reply));
        }
    }
    let created = hub
        .call(
            Method::POST,
            "/api/repos/create",
            Some(ALICE),
            Some(&serde_json::json!({"name": "n", "organization": "alice"})),
        )
        .await;
    urls.push(created.json()["url"].as_str().unwrap().to_owned());
    let committed = hub
        .commit(
            ALICE,
            "/api/models/alice/m",
            "main",
            &[header("again"), file("again.txt", b"again\n")],
        )
        .await;
    urls.push(committed.json()["commitUrl"].as_str().unwrap().to_owned());
    urls.sort();
    urls.dedup();
    let kinds = [
        "/v1/transfer/files/",
        "/xet-read-token/",
        "/v1/reconstructions/",
        "/tree/",
    ];
    for kind in kinds {
        assert!(
            urls.iter().any(|u| u.contains(kind)),
            "no {kind} URL in {urls:?}"
        );
    }

    for url in &urls {
        let path = url
            .strip_prefix(PUBLIC_URL)
            .unwrap_or_else(|| panic!("{url}"));
        for method in [Method::HEAD, Method::GET] {
            let reply = request(&router, method.clone(), path, &[]).await;
            // A web page the Hub links to but doesn't serve is a 404 of
            // the fallback; every route it serves needs the credential.
            let served = kinds.iter().any(|k| path.contains(k));
            let expected = if served {
                StatusCode::UNAUTHORIZED
            } else {
                StatusCode::NOT_FOUND
            };
            assert_eq!(reply.status, expected, "{method} {url}");
        }
    }
    // And the gateway with its credential in the wrong place.
    let gateway = urls
        .iter()
        .find(|u| u.contains("/v1/transfer/files/"))
        .unwrap();
    let path = gateway.strip_prefix(PUBLIC_URL).unwrap();
    let with_query = format!("{path}&access_token={ALICE}");
    let reply = request(&router, Method::GET, &with_query, &[]).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn duckdb_reads_through_resolve() {
    // Spike S4's recipe: DuckDB reads a private dataset's file through a
    // plain `resolve` URL with an `http` secret, following the `302` to
    // the gateway with its ranges. Runs only where `duckdb` (with httpfs)
    // is on PATH.
    let Ok(found) = std::process::Command::new("duckdb")
        .arg("-version")
        .output()
    else {
        eprintln!("skipped: duckdb is not on PATH");
        return;
    };
    assert!(found.status.success());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let mut cfg = TestHub::config();
    cfg.public_url = url::Url::parse(&origin).unwrap();
    cfg.cas_url = cfg.public_url.clone();
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(LocalBucket::open(&dir.path().join("bucket")).unwrap());
    let hub = TestHub::start_config(
        dir,
        bucket,
        support::users(),
        Arc::new(RulesAuthorizer::new(support::rules())),
        cfg,
    )
    .await;
    hub.create(ALICE, "alice", "d", Some("dataset")).await;
    let id = repo_id(&hub, "alice", "d").await;
    let mut csv = b"make,year\n".to_vec();
    for i in 0..20_000u32 {
        csv.extend_from_slice(
            format!(
                "{},{}\n",
                ["TESLA", "KIA", "BMW"][i as usize % 3],
                2000 + i % 25
            )
            .as_bytes(),
        );
    }
    let chunks: Vec<Vec<u8>> = csv.chunks(8 * 1024).map(<[u8]>::to_vec).collect();
    let n = u32::try_from(chunks.len()).unwrap();
    let stored = store_file(&hub, &id, &[chunks], &[(0, 0, n)]).await;
    let lines = [
        header("data"),
        lfs_file("data.csv", &stored.sha256, csv.len() as u64),
    ];
    let reply = hub
        .commit(ALICE, "/api/datasets/alice/d", "main", &lines)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let router = support::with_cas(&hub);
    let server = tokio::spawn(async move { axum::serve(listener, router).await });

    let sql = format!(
        "CREATE SECRET openxet (TYPE http, BEARER_TOKEN '{ALICE}', SCOPE '{origin}'); \
         SELECT make, count(*) FROM read_csv('{origin}/datasets/alice/d/resolve/main/data.csv') \
         GROUP BY make ORDER BY make;"
    );
    let output = tokio::time::timeout(
        std::time::Duration::from_mins(1),
        tokio::process::Command::new("duckdb")
            .args(["-csv", "-noheader", "-c", &sql])
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    server.abort();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // The first line is CREATE SECRET's `true`.
    assert_eq!(
        stdout.lines().skip(1).collect::<Vec<_>>(),
        ["BMW,6666", "KIA,6667", "TESLA,6667"]
    );
}
