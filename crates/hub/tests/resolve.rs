//! `HEAD|GET /{prefix}{ns}/{name}/resolve/{rev}/{path}` against spike S3's
//! huggingface.co fixtures (`docs/research/S3-hub-api-shapes.md`,
//! "resolve" and lesson 3): small files inline with their Git blob's
//! `ETag`, Xet files as a `302` to the file gateway with the headers
//! `huggingface_hub` reads, and `EntryNotFound` with `X-Repo-Commit`.

#![allow(clippy::unwrap_used)]

mod support;

use axum::body::Body;
use axum::http::header::{AUTHORIZATION, RANGE};
use axum::http::{Method, Request, StatusCode};
use openxet_hub::test_support::{BEARER, PUBLIC_URL, Reply, TestHub, fixture};
use serde_json::Value;
use support::{
    A_BIN_HASH, A_BIN_SHA256, ALICE, BOB, a_bin_size, by_path, commit_ok, file, header, hub,
    repo_id, send, uploaded,
};

/// `method path` as Alice, through `router`.
async fn as_alice(router: &axum::Router, method: Method, path: &str) -> Reply {
    send(router, method, path, ALICE, Vec::new()).await
}

/// The response headers of fixture `name`, by lowercase name.
fn fixture_headers(name: &str) -> Vec<(String, String)> {
    fixture(name)["response"]["headers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().unwrap().to_ascii_lowercase(),
                pair[1].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn fixture_header(name: &str, header: &str) -> String {
    fixture_headers(name)
        .into_iter()
        .find_map(|(n, v)| (n == header).then_some(v))
        .unwrap_or_else(|| panic!("{name} has no {header}"))
}

/// The blob id `tree` lists for `path` at `main`.
async fn blob_oid(hub: &TestHub, path: &str) -> String {
    let url = match path.rsplit_once('/') {
        Some((folder, _)) => format!("/api/models/alice/m/tree/main/{folder}"),
        None => "/api/models/alice/m/tree/main".to_owned(),
    };
    let tree = hub.call(Method::GET, &url, Some(ALICE), None).await;
    by_path(&tree.json(), path)["oid"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn resolve_small_file_inline() {
    let hub = hub().await;
    let (router, commit) = uploaded(&hub).await;
    for (fixture_name, path, content) in [
        (
            "26-hf-hub-download-small--01-HEAD-models-resolve.json",
            "config.json",
            &b"{\"hello\": \"openxet\"}\n"[..],
        ),
        (
            "33-hf-download-cli--01-HEAD-models-resolve.json",
            "sub/small.txt",
            &b"small file\n"[..],
        ),
    ] {
        let url = format!("/alice/m/resolve/main/{path}");
        let head = as_alice(&router, Method::HEAD, &url).await;
        assert_eq!(head.status, StatusCode::OK, "{path}: {}", head.text());
        assert!(head.body.is_empty());
        let oid = blob_oid(&hub, path).await;
        let name = path.rsplit('/').next().unwrap();
        // Every header S3 lists for a small file, as huggingface.co sends
        // it (fixtures `26`, `33`): the client reads `X-Repo-Commit`,
        // `ETag` and `Content-Length` (`file_download.py` lines
        // 1676-1688).
        let expected = [
            ("x-repo-commit", commit.clone()),
            ("etag", format!("\"{oid}\"")),
            ("content-length", content.len().to_string()),
            ("content-type", fixture_header(fixture_name, "content-type")),
            (
                "accept-ranges",
                fixture_header(fixture_name, "accept-ranges"),
            ),
            (
                "content-disposition",
                format!("inline; filename*=UTF-8''{name}; filename=\"{name}\";"),
            ),
            (
                "content-security-policy",
                fixture_header(fixture_name, "content-security-policy"),
            ),
        ];
        for (header, value) in &expected {
            assert_eq!(
                head.header(header),
                Some(value.as_str()),
                "{path}: {header}"
            );
        }
        assert_eq!(head.header("x-xet-hash"), None);
        assert_eq!(head.header("x-linked-size"), None);

        let get = as_alice(&router, Method::GET, &url).await;
        assert_eq!(get.status, StatusCode::OK);
        assert_eq!(&get.body[..], content);
        for (header, value) in &expected {
            assert_eq!(
                get.header(header),
                Some(value.as_str()),
                "{path}: GET {header}"
            );
        }
    }

    // Accept-Ranges is a promise: ranges of an inline file are served too,
    // as DuckDB and polars read small files (S4 G3, G5, G6, G9).
    let url = "/alice/m/resolve/main/config.json";
    let ranged = |range: &'static str| {
        let router = router.clone();
        async move {
            let request = Request::get(url)
                .header(AUTHORIZATION, format!("{BEARER} {ALICE}"))
                .header(RANGE, range)
                .body(Body::empty())
                .unwrap();
            send_request(&router, request).await
        }
    };
    let part = ranged("bytes=2-6").await;
    assert_eq!(part.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(&part.body[..], b"hello");
    assert_eq!(part.header("content-range"), Some("bytes 2-6/21"));
    let suffix = ranged("bytes=-2").await;
    assert_eq!(&suffix.body[..], b"}\n");
    let beyond = ranged("bytes=21-").await;
    assert_eq!(beyond.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(beyond.header("content-range"), Some("bytes */21"));
}

/// Sends `request` to `router`, read whole.
async fn send_request(router: &axum::Router, request: Request<Body>) -> Reply {
    use tower::ServiceExt as _;
    let response = router.clone().oneshot(request).await.unwrap();
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

#[tokio::test]
async fn resolve_xet_file_302_with_headers() {
    let hub = hub().await;
    let (router, commit) = uploaded(&hub).await;
    let id = repo_id(&hub, "alice", "m").await;
    let size = a_bin_size(&hub, &id).await;
    let reply = as_alice(&router, Method::HEAD, "/alice/m/resolve/main/big.bin").await;
    assert_eq!(reply.status, StatusCode::FOUND, "{}", reply.text());

    // Fixture `27-…-01`: the headers huggingface.co's 302 carries for a
    // Xet file, each with openxet's own value (S3 lesson 3). The
    // `Location` is the file gateway on the server's own origin (ruling
    // M2-R9), with the commit, so the gateway can answer it too.
    let fixture_name = "27-hf-hub-download-big--01-HEAD-models-resolve.json";
    let location =
        format!("{PUBLIC_URL}/v1/transfer/files/{id}/{A_BIN_HASH}?name=big.bin&commit={commit}");
    let link = format!(
        "<{PUBLIC_URL}/api/models/alice/m/xet-read-token/{commit}>; rel=\"xet-auth\", \
         <{PUBLIC_URL}/v1/reconstructions/{A_BIN_HASH}>; rel=\"xet-reconstruction-info\""
    );
    let expected = [
        ("location", location.clone()),
        ("x-repo-commit", commit.clone()),
        ("x-linked-size", size.to_string()),
        ("x-linked-etag", format!("\"{A_BIN_SHA256}\"")),
        ("x-xet-hash", A_BIN_HASH.to_owned()),
        ("link", link),
        (
            "accept-ranges",
            fixture_header(fixture_name, "accept-ranges"),
        ),
        (
            "cache-control",
            fixture_header(fixture_name, "cache-control"),
        ),
    ];
    for (name, value) in &expected {
        assert!(
            fixture_headers(fixture_name).iter().any(|(n, _)| n == name),
            "{name} is not a header of fixture 27"
        );
        assert_eq!(reply.header(name), Some(value.as_str()), "{name}");
    }

    // `huggingface_hub` follows a redirect on its own origin with the same
    // `HEAD` (`utils/_http.py` lines 764-779), and reads the metadata from
    // where it lands: the gateway repeats every value it needs.
    let followed = as_alice(&router, Method::HEAD, &location).await;
    assert_eq!(followed.status, StatusCode::OK, "{}", followed.text());
    assert_eq!(followed.header("x-repo-commit"), Some(commit.as_str()));
    assert_eq!(
        followed.header("etag"),
        Some(format!("\"{A_BIN_SHA256}\"").as_str())
    );
    assert_eq!(
        followed.header("content-length"),
        Some(size.to_string().as_str())
    );
    assert_eq!(followed.header("x-xet-hash"), Some(A_BIN_HASH));
    assert_eq!(
        followed.header("link"),
        reply.header("link"),
        "the same xet-auth and reconstruction links"
    );

    // The xet-auth link answers a read token, as `hf_xet` fetches it.
    let token_url = format!("/api/models/alice/m/xet-read-token/{commit}");
    let token = as_alice(&router, Method::GET, &token_url).await;
    assert_eq!(token.status, StatusCode::OK, "{}", token.text());

    // A GET follows to the gateway and reads the file: the bytes `hf_xet`
    // 1.7.0 uploaded, by their SHA-256.
    let get = as_alice(&router, Method::GET, &location).await;
    assert_eq!(get.status, StatusCode::OK);
    assert_eq!(get.body.len() as u64, size);
    assert_eq!(sha256_hex(&get.body), A_BIN_SHA256);
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    use std::fmt::Write as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, b| {
            write!(out, "{b:02x}").unwrap();
            out
        })
}

#[tokio::test]
async fn resolve_404_carries_repo_commit() {
    let hub = hub().await;
    let (router, commit) = uploaded(&hub).await;
    let fixture_name = "28-hf-hub-download-entry-not-found--01-HEAD-models-resolve.json";
    for method in [Method::HEAD, Method::GET] {
        let reply = as_alice(&router, method.clone(), "/alice/m/resolve/main/missing.txt").await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(
            reply.header("x-error-code"),
            Some(fixture_header(fixture_name, "x-error-code").as_str())
        );
        // `file_download.py` line 1773 caches the miss under this commit.
        assert_eq!(reply.header("x-repo-commit"), Some(commit.as_str()));
        assert!(reply.header("x-error-message").is_some());
        if method == Method::GET {
            assert_eq!(reply.json()["code"], "entry_not_found");
        }
    }
    // An unknown revision is a RevisionNotFound, as on the other reads.
    let reply = as_alice(&router, Method::HEAD, "/alice/m/resolve/nope/config.json").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.header("x-error-code"), Some("RevisionNotFound"));

    // A pointer whose file was never uploaded here (a `git push` can carry
    // one): EntryNotFound, with a message saying so, never a redirect.
    let pointer = openxet_git::canonical_pointer(&[9; 32], 1234);
    let commit = commit_ok(
        &hub,
        "alice/m",
        "main",
        &[header("orphan"), file("orphan.bin", &pointer)],
    )
    .await;
    let reply = as_alice(&router, Method::GET, "/alice/m/resolve/main/orphan.bin").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.text());
    assert_eq!(reply.header("x-error-code"), Some("EntryNotFound"));
    assert_eq!(reply.header("x-repo-commit"), Some(commit.as_str()));
    assert_eq!(reply.json()["code"], "lfs_object_missing");
    assert!(reply.header("location").is_none());
}

#[tokio::test]
async fn resolve_path_edges() {
    // Review Focus 4: odd paths answer the entry or EntryNotFound with
    // X-Repo-Commit, never a 500 or anything outside the commit's tree.
    let hub = hub().await;
    let (router, _) = uploaded(&hub).await;
    let commit = commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("odd paths"),
            file("with space.txt", b"spaced\n"),
            file("ünïcödé.txt", b"utf-8\n"),
            file("dir/inner.txt", b"inner\n"),
        ],
    )
    .await;
    let found = [
        ("with%20space.txt", &b"spaced\n"[..]),
        ("%C3%BCn%C3%AFc%C3%B6d%C3%A9.txt", b"utf-8\n"),
        ("dir%2Finner.txt", b"inner\n"),
        ("/config.json", b"{\"hello\": \"openxet\"}\n"),
        ("dir//inner.txt", b"inner\n"),
    ];
    for (path, content) in found {
        let reply = as_alice(
            &router,
            Method::GET,
            &format!("/alice/m/resolve/main/{path}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text());
        assert_eq!(&reply.body[..], content, "{path}");
        assert_eq!(reply.header("x-repo-commit"), Some(commit.as_str()));
    }
    let disposition = as_alice(
        &router,
        Method::HEAD,
        "/alice/m/resolve/main/%C3%BCn%C3%AFc%C3%B6d%C3%A9.txt",
    )
    .await;
    assert_eq!(
        disposition.header("content-disposition"),
        Some(
            "inline; filename*=UTF-8''%C3%BCn%C3%AFc%C3%B6d%C3%A9.txt; \
             filename=\"_n_c_d_.txt\";"
        )
    );
    let missing = [
        "./config.json",
        "dir/../config.json",
        "..%2F..%2Fetc%2Fpasswd",
        "%2E%2E/config.json",
        "dir",
        "dir/",
        "sub",
        "config.json/x",
        "nope/inner.txt",
        "",
    ];
    for path in missing {
        let reply = as_alice(
            &router,
            Method::GET,
            &format!("/alice/m/resolve/main/{path}"),
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::NOT_FOUND,
            "{path:?}: {}",
            reply.text()
        );
        assert_eq!(
            reply.header("x-error-code"),
            Some("EntryNotFound"),
            "{path:?}"
        );
        assert_eq!(
            reply.header("x-repo-commit"),
            Some(commit.as_str()),
            "{path:?}"
        );
    }
    // A commit id is a revision too, and names its own commit.
    let at = as_alice(
        &router,
        Method::HEAD,
        &format!("/alice/m/resolve/{commit}/with%20space.txt"),
    )
    .await;
    assert_eq!(at.status, StatusCode::OK);
    assert_eq!(at.header("x-repo-commit"), Some(commit.as_str()));
}

#[tokio::test]
async fn resolve_through_typed_prefixes() {
    // `datasets/…` is the dataset view (fixture `43`); a repository
    // without the label answers the wrong-view error, never its content.
    let hub = hub().await;
    hub.create(ALICE, "alice", "d", Some("dataset")).await;
    let lines = [header("data"), file("data.csv", b"a,b\n1,2\n")];
    let reply = hub
        .commit(ALICE, "/api/datasets/alice/d", "main", &lines)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let router = support::with_cas(&hub);
    let dataset = as_alice(
        &router,
        Method::GET,
        "/datasets/alice/d/resolve/main/data.csv",
    )
    .await;
    assert_eq!(dataset.status, StatusCode::OK);
    assert_eq!(&dataset.body[..], b"a,b\n1,2\n");
    let model = as_alice(&router, Method::GET, "/alice/d/resolve/main/data.csv").await;
    assert_eq!(model.status, StatusCode::NOT_FOUND);
    assert_eq!(model.json()["code"], "wrong_label");
}

#[tokio::test]
async fn resolve_anonymous_private_401_shape() {
    // Fixture `35`: without a credential, a private repository's file is
    // a 401 with `WWW-Authenticate` and `X-Error-Message`, which
    // `huggingface_hub` raises as RepositoryNotFoundError, the same
    // whether the repository exists or not.
    let hub = hub().await;
    let (router, _) = uploaded(&hub).await;
    let fixture_name = "35-anonymous-download-private--01-HEAD-models-resolve.json";
    assert_eq!(
        fixture(fixture_name)["response"]["status"],
        Value::from(401)
    );
    let anonymous = |path: &'static str| {
        let router = router.clone();
        async move { send_request(&router, Request::head(path).body(Body::empty()).unwrap()).await }
    };
    let existing = anonymous("/alice/m/resolve/main/config.json").await;
    assert_eq!(existing.status, StatusCode::UNAUTHORIZED);
    for name in ["www-authenticate", "x-error-message"] {
        assert!(
            fixture_headers(fixture_name).iter().any(|(n, _)| n == name),
            "{name}"
        );
        assert!(existing.header(name).is_some(), "{name}");
    }
    assert!(existing.header("x-repo-commit").is_none());
    let missing = anonymous("/alice/gone/resolve/main/config.json").await;
    assert_eq!(existing.comparable(), missing.comparable());

    // A caller who may not read it gets the unknown-repository answer.
    let carol = send(
        &router,
        Method::HEAD,
        "/alice/m/resolve/main/config.json",
        support::CAROL,
        Vec::new(),
    )
    .await;
    assert_eq!(carol.status, StatusCode::NOT_FOUND);
    assert_eq!(carol.header("x-error-code"), Some("RepoNotFound"));
    // Bob may read it.
    let bob = send(
        &router,
        Method::HEAD,
        "/alice/m/resolve/main/config.json",
        BOB,
        Vec::new(),
    )
    .await;
    assert_eq!(bob.status, StatusCode::OK);
}
