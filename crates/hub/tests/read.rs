//! Reads on the Hub API: tree, paths-info, commits and repository info at
//! a revision, against S3's huggingface.co fixtures, and their pagination
//! (ruling M2-R8).

#![allow(clippy::unwrap_used)]

mod support;

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use openxet_git::{HubCommit, Op, canonical_pointer};
use openxet_hub::test_support::{PUBLIC_URL, Reply, TestHub, assert_read_keys, fixture_json};
use serde_json::{Value, json};
use support::{
    A_BIN_HASH, A_BIN_SHA256, ALICE, DEFAULT_GITATTRIBUTES_OID, by_path, commit_ok, file, header,
    hub, paths, repo_id, uploaded,
};

async fn get(hub: &TestHub, path: &str) -> Reply {
    hub.call(
        Method::GET,
        &format!("/api/models/alice/m/{path}"),
        Some(ALICE),
        None,
    )
    .await
}

async fn ok(hub: &TestHub, path: &str) -> Value {
    let reply = get(hub, path).await;
    assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text());
    reply.json()
}

const FILE_KEYS: &[&str] = &["type", "oid", "size", "path"];
const LFS_KEYS: &[&str] = &[
    "type",
    "oid",
    "size",
    "path",
    "lfs.oid",
    "lfs.size",
    "lfs.pointerSize",
    "xetHash",
];

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "every fixture tree shape against one upload"
)]
async fn tree_shapes() {
    let hub = hub().await;
    let (_router, _) = uploaded(&hub).await;
    // `list_repo_tree` sends both parameters, always (`hf_api.py` line 3969).
    let top = ok(&hub, "tree/main?recursive=false&expand=false").await;
    let expected = fixture_json("16-list-repo-tree--01-GET-models-tree.json");
    assert_eq!(
        paths(&top),
        [
            ".gitattributes",
            "README.md",
            "big.bin",
            "config.json",
            "sub"
        ]
    );
    assert_read_keys(&expected[0], by_path(&top, ".gitattributes"), FILE_KEYS);
    assert_eq!(
        by_path(&top, ".gitattributes")["oid"],
        DEFAULT_GITATTRIBUTES_OID
    );
    assert_eq!(by_path(&top, ".gitattributes")["size"], 1519);
    assert_read_keys(&expected[2], by_path(&top, "big.bin"), LFS_KEYS);
    assert_eq!(by_path(&top, "big.bin")["xetHash"], A_BIN_HASH);
    assert_eq!(by_path(&top, "big.bin")["lfs"]["oid"], A_BIN_SHA256);
    let sub = by_path(&top, "sub");
    assert_eq!(sub["type"], "directory");
    assert_eq!(sub["oid"].as_str().unwrap().len(), 40);
    // `list_repo_files`: everything, recursively (S3 fixture `19`).
    let all = ok(&hub, "tree/main?recursive=true&expand=false").await;
    assert_eq!(
        paths(&all),
        [
            ".gitattributes",
            "README.md",
            "big.bin",
            "config.json",
            "sub",
            "sub/small.txt"
        ]
    );
    // A folder's entries.
    let inner = ok(&hub, "tree/main/sub").await;
    assert_eq!(paths(&inner), ["sub/small.txt"]);
    assert_read_keys(&expected[1], by_path(&inner, "sub/small.txt"), FILE_KEYS);

    // A missing path: huggingface.co's text, `EntryNotFound` and the
    // commit, which `huggingface_hub` caches the miss under (S3 fixture
    // `18`).
    let created = hub
        .call(
            Method::POST,
            "/api/models/alice/m/branch/s3-branch",
            Some(ALICE),
            Some(&json!({})),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK);
    commit_ok(
        &hub,
        "alice/m",
        "s3-branch",
        &[
            header("drop sub"),
            json!({"key": "deletedFolder", "value": {"path": "sub"}}),
        ],
    )
    .await;
    let missing = get(&hub, "tree/s3-branch/sub?recursive=false&expand=false").await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    let text = "sub does not exist on \"s3-branch\"";
    assert_eq!(missing.header("x-error-code"), Some("EntryNotFound"));
    assert_eq!(missing.header("x-error-message"), Some(text));
    assert_eq!(missing.json()["error"], text);
    assert_read_keys(
        &fixture_json("18-list-repo-tree-subpath--01-GET-models-tree-sub.json"),
        &missing.json(),
        &["error"],
    );
    let branch_tip = ok(&hub, "refs").await["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "s3-branch")
        .unwrap()["targetCommit"]
        .clone();
    assert_eq!(json!(missing.header("x-repo-commit").unwrap()), branch_tip);
    // A missing revision.
    let nowhere = get(&hub, "tree/no-such-rev").await;
    assert_eq!(nowhere.status, StatusCode::NOT_FOUND);
    assert_eq!(nowhere.header("x-error-code"), Some("RevisionNotFound"));

    // A pointer whose SHA-256 this repository never registered lists as
    // LFS, without `xetHash`.
    let id = repo_id(&hub, "alice", "m").await.parse().unwrap();
    let pointer = canonical_pointer(&[9; 32], 5);
    let alice = openxet_auth::UserPrincipal {
        subject: "alice".to_owned(),
        name: "alice".to_owned(),
        email: None,
        claims: std::collections::BTreeMap::default(),
    };
    hub.state
        .engine
        .hub_commit(
            id,
            &alice,
            HubCommit {
                branch: "main",
                parent: None,
                summary: "pushed pointer",
                description: "",
                ops: vec![Op::File {
                    path: "foreign.bin".to_owned(),
                    content: Bytes::from(pointer),
                }],
            },
        )
        .await
        .unwrap();
    let top = ok(&hub, "tree/main").await;
    let foreign = by_path(&top, "foreign.bin");
    assert_eq!(foreign["lfs"]["oid"], "09".repeat(32));
    assert_eq!(foreign["size"], 5);
    assert!(foreign.get("xetHash").is_none(), "{foreign}");
}

#[tokio::test]
async fn tree_expand_last_commit() {
    let hub = hub().await;
    let (_, upload) = uploaded(&hub).await;
    let later = commit_ok(
        &hub,
        "alice/m",
        "main",
        &[header("S3 delete + add"), file("extra.txt", b"extra\n")],
    )
    .await;
    let all = ok(&hub, "tree/main?recursive=true&expand=true").await;
    let expected = fixture_json("17-list-repo-tree-recursive-expand--01-GET-models-tree.json");
    let keys = [
        "lastCommit.id",
        "lastCommit.title",
        "lastCommit.date",
        "securityFileStatus.status",
        "securityFileStatus.avScan.status",
        "securityFileStatus.pickleImportScan.status",
    ];
    for entry in all.as_array().unwrap() {
        if entry["type"] == "file" {
            assert_read_keys(&expected[1], entry, &keys);
        }
    }
    let last = |path: &str| by_path(&all, path)["lastCommit"].clone();
    assert_eq!(last(".gitattributes")["title"], "initial commit");
    assert_eq!(last("README.md")["id"], upload);
    assert_eq!(last("README.md")["title"], "S3 upload_folder");
    assert_eq!(last("extra.txt")["id"], later);
    let date = last("extra.txt")["date"].as_str().unwrap().to_owned();
    assert_eq!(date.len(), "2026-10-23T05:02:15.000Z".len(), "{date}");
    assert_eq!(by_path(&all, "sub")["lastCommit"]["id"], upload);
}

#[tokio::test]
async fn tree_path_edges() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("edges"),
            file("with space.txt", b"s"),
            file("ü/ñ.txt", b"u"),
            file("a/b/c.txt", b"c"),
        ],
    )
    .await;
    let created = hub
        .call(
            Method::POST,
            "/api/models/alice/m/branch/feat%2Fx",
            Some(ALICE),
            Some(&json!({})),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    let listing = |path: &str, want: &[&str]| {
        let hub = &hub;
        let path = path.to_owned();
        let want: Vec<String> = want.iter().map(|s| (*s).to_owned()).collect();
        async move {
            let reply = get(hub, &path).await;
            assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text());
            assert_eq!(paths(&reply.json()), want, "{path}");
        }
    };
    let not_found = |path: &str| {
        let hub = &hub;
        let path = path.to_owned();
        async move {
            let reply = get(hub, &path).await;
            assert_eq!(
                reply.status,
                StatusCode::NOT_FOUND,
                "{path}: {}",
                reply.text()
            );
            assert_eq!(
                reply.header("x-error-code"),
                Some("EntryNotFound"),
                "{path}"
            );
            assert_eq!(
                reply.header("x-repo-commit").map(str::len),
                Some(40),
                "{path}"
            );
        }
    };
    listing("tree/main/%C3%BC", &["ü/ñ.txt"]).await;
    listing("tree/main/a%2Fb", &["a/b/c.txt"]).await;
    listing("tree/main//a", &["a/b"]).await;
    listing("tree/main/a/", &["a/b"]).await;
    // A revision with a `/`, percent-encoded as `huggingface_hub` sends it.
    listing("tree/feat%2Fx/a/b", &["a/b/c.txt"]).await;
    // A file is no folder to list.
    not_found("tree/main/with%20space.txt").await;
    // `.` and `..` never step anywhere, inside or outside the tree.
    for path in [
        "tree/main/a/./b",
        "tree/main/a/../a",
        "tree/main/..",
        "tree/main/%2E%2E",
        "tree/main/a/%2E%2E/%2E%2E/etc",
    ] {
        not_found(path).await;
    }
    // paths-info answers the same entries, and leaves the rest out.
    let reply = hub
        .call_raw(
            Method::POST,
            "/api/models/alice/m/paths-info/main",
            Some(ALICE),
            "application/x-www-form-urlencoded",
            "paths=with+space.txt&paths=%C3%BC%2F%C3%B1.txt&paths=%2Fa%2Fb&paths=a%2F..%2Fa&paths=..",
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(paths(&reply.json()), ["with space.txt", "ü/ñ.txt", "a/b"]);
}

#[tokio::test]
async fn paths_info_omits_missing() {
    let hub = hub().await;
    let (_router, _) = uploaded(&hub).await;
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("drop sub"),
            json!({"key": "deletedFolder", "value": {"path": "sub"}}),
        ],
    )
    .await;
    // The fixture's form body (S3 fixture `20`): `sub` no longer exists and
    // `missing.txt` never did; both are left out.
    let reply = hub
        .call_raw(
            Method::POST,
            "/api/models/alice/m/paths-info/main",
            Some(ALICE),
            "application/x-www-form-urlencoded",
            "paths=README.md&paths=big.bin&paths=sub&paths=missing.txt&expand=true",
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let entries = reply.json();
    assert_eq!(paths(&entries), ["README.md", "big.bin"]);
    let expected = fixture_json("20-get-paths-info--01-POST-models-paths-info.json");
    assert_read_keys(
        &expected[0],
        &entries[0],
        &["type", "oid", "size", "path", "lastCommit.id"],
    );
    assert_read_keys(&expected[1], &entries[1], LFS_KEYS);
    assert_read_keys(
        &expected[1],
        &entries[1],
        &["lastCommit.title", "securityFileStatus.status"],
    );
    // Without `expand`, neither expanded field.
    let plain = hub
        .call_raw(
            Method::POST,
            "/api/models/alice/m/paths-info/main",
            Some(ALICE),
            "application/x-www-form-urlencoded",
            "paths=README.md",
        )
        .await
        .json();
    assert!(plain[0].get("lastCommit").is_none(), "{plain}");
}

/// More than 1,000 `paths` fields are refused while the body is parsed,
/// repeats of one path included, as `get_paths_info` sends at most 1,000.
#[tokio::test]
async fn paths_info_refuses_more_than_1000_paths() {
    let hub = hub().await;
    let (_router, _) = uploaded(&hub).await;
    let post = |body: String| {
        hub.call_raw(
            Method::POST,
            "/api/models/alice/m/paths-info/main",
            Some(ALICE),
            "application/x-www-form-urlencoded",
            body,
        )
    };
    let distinct = |n: usize| {
        (0..n)
            .map(|i| format!("paths=f{i}"))
            .collect::<Vec<_>>()
            .join("&")
    };
    assert_eq!(post(distinct(1000)).await.status, StatusCode::OK);
    let over = post(distinct(1001)).await;
    assert_eq!(over.status, StatusCode::BAD_REQUEST, "{}", over.text());
    assert!(
        over.text().contains("at most 1000 paths"),
        "{}",
        over.text()
    );
    let repeated = vec!["paths=README.md"; 1001].join("&");
    assert_eq!(post(repeated).await.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn commits_newest_first_with_total() {
    let hub = hub().await;
    let (_, upload) = uploaded(&hub).await;
    let later = commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            json!({"key": "header", "value": {"summary": "S3 delete + add", "description": "why\n\nmore"}}),
            file("extra.txt", b"extra\n"),
        ],
    )
    .await;
    let reply = get(&hub, "commits/main").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.header("x-total-count"), Some("3"));
    let log = reply.json();
    let expected = fixture_json("21-list-repo-commits--01-GET-models-commits.json");
    for item in log.as_array().unwrap() {
        assert_read_keys(
            &expected[0],
            item,
            &["id", "title", "message", "authors[].user", "date"],
        );
    }
    let ids: Vec<&str> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids[..2], [later.as_str(), upload.as_str()]);
    assert_eq!(log[0]["title"], "S3 delete + add");
    assert_eq!(log[0]["message"], "why\n\nmore");
    assert_eq!(log[0]["authors"][0]["user"], "alice");
    // The server-generated first commit (ruling M2-R5, S3 fixture `21`).
    assert_eq!(log[2]["title"], "initial commit");
    assert_eq!(log[2]["message"], "");
    assert_eq!(log[2]["authors"][0]["user"], "openxet");
    // At an earlier commit, its own history.
    let earlier = get(&hub, &format!("commits/{upload}")).await;
    assert_eq!(earlier.header("x-total-count"), Some("2"));
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "every fixture repo info shape against one upload"
)]
async fn repo_info_shapes() {
    let hub = hub().await;
    let (_, upload) = uploaded(&hub).await;
    let info = hub
        .call(Method::GET, "/api/models/alice/m", Some(ALICE), None)
        .await;
    assert_eq!(info.status, StatusCode::OK, "{}", info.text());
    let info = info.json();
    assert_read_keys(
        &fixture_json("22-model-info--01-GET-models.json"),
        &info,
        &[
            "_id",
            "id",
            "private",
            "tags",
            "downloads",
            "likes",
            "modelId",
            "author",
            "sha",
            "lastModified",
            "gated",
            "disabled",
            "config",
            "siblings[].rfilename",
            "spaces",
            "createdAt",
        ],
    );
    assert!(info.get("usedStorage").is_none());
    // `snapshot_download` asserts `sha` (`_snapshot_download.py` line 280).
    assert_eq!(info["sha"], upload);
    assert_eq!(info["tags"], json!([]));
    assert_eq!(info["labels"], json!(["model"]));
    assert_eq!(info["author"], "alice");
    let names: Vec<&str> = info["siblings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["rfilename"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            ".gitattributes",
            "README.md",
            "big.bin",
            "config.json",
            "sub/small.txt"
        ]
    );
    assert!(info["siblings"][0].get("blobId").is_none());

    // At a revision, with blobs (S3 fixture `23`): the sibling names the
    // SHA-256 `sha256`, where the tree names it `oid`.
    let tagged = hub
        .call(
            Method::POST,
            "/api/models/alice/m/tag/main",
            Some(ALICE),
            Some(&json!({"tag": "v0-s3"})),
        )
        .await;
    assert_eq!(tagged.status, StatusCode::OK);
    commit_ok(&hub, "alice/m", "main", &[header("later"), file("x", b"x")]).await;
    let at = hub
        .call(
            Method::GET,
            "/api/models/alice/m/revision/v0-s3?blobs=true",
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(at.status, StatusCode::OK, "{}", at.text());
    let at = at.json();
    assert_eq!(at["sha"], upload);
    let expected =
        fixture_json("23-model-info-files-metadata-revision--01-GET-models-revision.json");
    let sibling = |json: &Value, name: &str| {
        json["siblings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["rfilename"] == name)
            .unwrap()
            .clone()
    };
    assert_read_keys(
        &sibling(&expected, "README.md"),
        &sibling(&at, "README.md"),
        &["rfilename", "blobId", "size"],
    );
    let big = sibling(&at, "big.bin");
    assert_read_keys(
        &sibling(&expected, "big.bin"),
        &big,
        &[
            "rfilename",
            "blobId",
            "size",
            "lfs.sha256",
            "lfs.size",
            "lfs.pointerSize",
        ],
    );
    assert_eq!(big["lfs"]["sha256"], A_BIN_SHA256);
    assert!(big["lfs"].get("oid").is_none());
    assert_eq!(
        sibling(&at, ".gitattributes")["blobId"],
        DEFAULT_GITATTRIBUTES_OID
    );
    assert!(sibling(&at, "README.md").get("lfs").is_none());

    // A missing revision: huggingface.co's text (S3 fixture `25`).
    let missing = hub
        .call(
            Method::GET,
            "/api/models/alice/m/revision/no-such-rev",
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.header("x-error-code"), Some("RevisionNotFound"));
    assert_eq!(
        missing.header("x-error-message"),
        Some("Invalid rev id: no-such-rev")
    );
    assert_read_keys(
        &fixture_json("25-revision-not-found--01-GET-models-revision.json"),
        &missing.json(),
        &["error"],
    );
    // A dataset answers the same core (S3 fixture `42`).
    hub.create(ALICE, "alice", "d", Some("dataset")).await;
    let dataset = hub
        .call(Method::GET, "/api/datasets/alice/d", Some(ALICE), None)
        .await
        .json();
    assert_eq!(dataset["siblings"][0]["rfilename"], ".gitattributes");
    assert_eq!(dataset["sha"].as_str().unwrap().len(), 40);
}

/// Follows `reply`'s `Link: <…>; rel="next"` to the end, collecting each
/// page's entries.
async fn follow(hub: &TestHub, first: &str) -> (Vec<Value>, usize) {
    let mut path = format!("/api/models/alice/m/{first}");
    let mut all = Vec::new();
    let mut pages = 0;
    loop {
        let reply = hub.call(Method::GET, &path, Some(ALICE), None).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}: {}", reply.text());
        pages += 1;
        all.extend(reply.json().as_array().unwrap().iter().cloned());
        let Some(link) = reply.header("link") else {
            return (all, pages);
        };
        let url = link
            .strip_prefix('<')
            .and_then(|l| l.strip_suffix(">; rel=\"next\""))
            .unwrap();
        url.strip_prefix(PUBLIC_URL).unwrap().clone_into(&mut path);
    }
}

#[tokio::test]
async fn listings_paginate_with_link_next() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    for i in 0..3 {
        commit_ok(
            &hub,
            "alice/m",
            "main",
            &[header(&format!("c{i}")), file(&format!("d/f{i}.txt"), b"x")],
        )
        .await;
    }
    let full = ok(&hub, "tree/main?recursive=true").await;
    assert_eq!(full.as_array().unwrap().len(), 5);
    // Two at a time: three pages, every entry once, in order, and the
    // request's other parameters kept.
    let (paged, page_count) = follow(&hub, "tree/main?recursive=true&limit=2").await;
    assert_eq!(page_count, 3);
    assert_eq!(Value::Array(paged), full);
    let first = get(&hub, "tree/main?recursive=true&limit=2").await;
    let link = first.header("link").unwrap();
    assert!(
        link.starts_with(&format!("<{PUBLIC_URL}/api/models/alice/m/tree/main?")),
        "{link}"
    );
    assert!(
        link.contains("recursive=true") && link.contains("cursor="),
        "{link}"
    );
    // Commits too: newest first across pages.
    let all = ok(&hub, "commits/main").await;
    let (paged, page_count) = follow(&hub, "commits/main?limit=1").await;
    assert_eq!(page_count, 4);
    assert_eq!(Value::Array(paged), all);
    // Only the first page counts the history.
    let first = get(&hub, "commits/main?limit=1").await;
    assert_eq!(first.header("x-total-count"), Some("4"));
    let next = first.header("link").unwrap();
    let next = next
        .strip_prefix(&format!("<{PUBLIC_URL}"))
        .and_then(|l| l.strip_suffix(">; rel=\"next\""))
        .unwrap();
    let second = hub.call(Method::GET, next, Some(ALICE), None).await;
    assert_eq!(second.status, StatusCode::OK);
    assert!(second.header("x-total-count").is_none());
    // A page of everything links nothing.
    assert!(get(&hub, "commits/main").await.header("link").is_none());
    // A cursor this server didn't issue.
    let bad = get(&hub, "tree/main?cursor=%%%").await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}
