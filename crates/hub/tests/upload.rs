//! Uploads on the Hub API: preupload (ruling M2-R4), commits (ruling M2-R7),
//! branches and tags, against S3's huggingface.co fixtures.

#![allow(clippy::unwrap_used)]

mod support;

use std::process::Command;
use std::sync::Arc;

use axum::http::{Method, StatusCode};
use openxet_auth::RulesAuthorizer;
use openxet_hub::test_support::{
    PUBLIC_URL, Reply, TestHub, assert_read_keys, fixture, fixture_json,
};
use openxet_hub::{HubConfig, MAX_COMMIT_OPS};
use openxet_storage::LocalBucket;
use serde_json::{Value, json};
use support::{
    A_BIN_HASH, A_BIN_SHA256, ALICE, BOB, a_bin_size, by_path, commit_ok, file, header, hub,
    lfs_file, paths, repo_id, upload_capture, uploaded, with_cas,
};

async fn preupload(hub: &TestHub, repo: &str, rev: &str, files: &[(&str, u64)]) -> Reply {
    let files: Vec<Value> = files
        .iter()
        .map(|(path, size)| json!({"path": path, "sample": "", "size": size}))
        .collect();
    hub.call(
        Method::POST,
        &format!("/api/models/{repo}/preupload/{rev}"),
        Some(ALICE),
        Some(&json!({ "files": files })),
    )
    .await
}

/// The upload mode preupload answers per path.
fn modes(reply: &Reply) -> Vec<(String, String)> {
    modes_of(&reply.json())
}

fn modes_of(json: &Value) -> Vec<(String, String)> {
    json["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["uploadMode"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn mode(reply: &Reply, path: &str) -> String {
    modes(reply)
        .into_iter()
        .find(|(p, _)| p == path)
        .map(|(_, m)| m)
        .unwrap()
}

async fn tree(hub: &TestHub, path: &str) -> Value {
    let reply = hub
        .call(
            Method::GET,
            &format!("/api/models/alice/m/tree/{path}"),
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    reply.json()
}

async fn refs(hub: &TestHub) -> Value {
    hub.call(Method::GET, "/api/models/alice/m/refs", Some(ALICE), None)
        .await
        .json()
}

fn main_tip(refs: &Value) -> String {
    refs["branches"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "main")
        .unwrap()["targetCommit"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one repository, every routing rule of M2-R4"
)]
async fn preupload_routes_by_size_and_gitattributes() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let threshold = HubConfig::DEFAULT_LFS_THRESHOLD;
    // The fixture's files, decided as huggingface.co decided them.
    let request =
        &fixture("09-upload-folder--02-POST-models-preupload.json")["request"]["body"]["json"];
    let reply = hub
        .call(
            Method::POST,
            "/api/models/alice/m/preupload/main",
            Some(ALICE),
            Some(request),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_read_keys(
        &fixture_json("09-upload-folder--02-POST-models-preupload.json"),
        &reply.json(),
        &[
            "files[].path",
            "files[].uploadMode",
            "files[].shouldIgnore",
            "commitOid",
        ],
    );
    assert_eq!(
        modes(&reply),
        modes_of(&fixture_json(
            "09-upload-folder--02-POST-models-preupload.json"
        ))
    );
    assert_eq!(reply.json()["commitOid"], main_tip(&refs(&hub).await));

    // By size, whatever the name; by the generated `.gitattributes`'
    // patterns, whatever the size.
    let reply = preupload(
        &hub,
        "alice/m",
        "main",
        &[
            ("data.csv", threshold),
            ("data2.csv", threshold - 1),
            ("w/model.safetensors", 10),
            ("x.bin", 10),
            ("README.md", 20),
        ],
    )
    .await;
    assert_eq!(
        modes(&reply),
        [
            ("data.csv", "lfs"),
            ("data2.csv", "regular"),
            ("w/model.safetensors", "lfs"),
            ("x.bin", "lfs"),
            ("README.md", "regular"),
        ]
        .map(|(p, m)| (p.to_owned(), m.to_owned()))
    );

    // The target revision's `.gitattributes` decides: a branch keeps the
    // default while `main` routes `*.csv` and no longer `*.bin`.
    let created = hub
        .call(
            Method::POST,
            "/api/models/alice/m/branch/old",
            Some(ALICE),
            Some(&json!({})),
        )
        .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("route csv"),
            file(
                ".gitattributes",
                b"*.csv filter=lfs diff=lfs merge=lfs -text\n",
            ),
        ],
    )
    .await;
    let on_main = preupload(&hub, "alice/m", "main", &[("a.csv", 5), ("x.bin", 5)]).await;
    assert_eq!(mode(&on_main, "a.csv"), "lfs");
    assert_eq!(mode(&on_main, "x.bin"), "regular");
    let on_old = preupload(&hub, "alice/m", "old", &[("a.csv", 5), ("x.bin", 5)]).await;
    assert_eq!(mode(&on_old, "a.csv"), "regular");
    assert_eq!(mode(&on_old, "x.bin"), "lfs");
    // Without `.gitattributes`, only size routes.
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("no attributes"),
            json!({"key": "deletedFile", "value": {"path": ".gitattributes"}}),
        ],
    )
    .await;
    let bare = preupload(&hub, "alice/m", "main", &[("x.bin", 5), ("y", threshold)]).await;
    assert_eq!(mode(&bare, "x.bin"), "regular");
    assert_eq!(mode(&bare, "y"), "lfs");
    // Preupload prepares a commit: it needs `write`.
    let bob = hub
        .call(
            Method::POST,
            "/api/models/alice/m/preupload/main",
            Some(BOB),
            Some(&json!({"files": []})),
        )
        .await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn zero_byte_and_tiny_files_regular() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let reply = preupload(
        &hub,
        "alice/m",
        "main",
        &[
            ("empty.bin", 0),
            ("one.txt", 1),
            ("extra.txt", 6),
            ("empty", 0),
        ],
    )
    .await;
    // `huggingface_hub` forces zero-byte files to regular itself
    // (`_commit_api.py` line 784); openxet answers the same.
    for (path, mode) in modes(&reply) {
        assert_eq!(mode, "regular", "{path}");
    }
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("empty"),
            file("empty.bin", b""),
            file("one.txt", b"1"),
        ],
    )
    .await;
    let tree = tree(&hub, "main").await;
    assert_eq!(by_path(&tree, "empty.bin")["size"], 0);
    assert!(by_path(&tree, "empty.bin").get("lfs").is_none());
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one commit of every operation, then every read of it"
)]
async fn commit_round_trip() {
    let hub = hub().await;
    let (_, oid) = uploaded(&hub).await;
    // The answer `create_commit` reads (`_commit_api.py` lines 1074–1077).
    let tip = main_tip(&refs(&hub).await);
    assert_eq!(oid, tip);
    let reply = hub
        .commit(
            ALICE,
            "/api/models/alice/m",
            "main",
            &[header("S3 delete + add")]
                .into_iter()
                .chain([
                    json!({"key": "deletedFile", "value": {"path": "sub/small.txt"}}),
                    file("extra.txt", b"extra\n"),
                ])
                .collect::<Vec<_>>(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_read_keys(
        &fixture_json("10-create-commit-delete--02-POST-models-commit.json"),
        &body,
        &["success", "commitOid", "commitUrl", "hookOutput"],
    );
    let second = body["commitOid"].as_str().unwrap();
    assert_eq!(
        body["commitUrl"],
        format!("{PUBLIC_URL}/alice/m/commit/{second}")
    );
    assert_eq!(
        (body["success"].clone(), body["hookOutput"].clone()),
        (json!(true), json!(""))
    );

    // The tree is the fixture's (S3 fixture `16`): the same blob ids for
    // the same contents, and `big.bin` an LFS pointer naming its Xet file.
    let tree = tree(&hub, "main?recursive=true").await;
    assert_eq!(
        paths(&tree),
        [
            ".gitattributes",
            "README.md",
            "big.bin",
            "config.json",
            "extra.txt"
        ]
    );
    let expected = fixture_json("16-list-repo-tree--01-GET-models-tree.json");
    for path in [".gitattributes", "README.md", "config.json", "extra.txt"] {
        assert_eq!(
            by_path(&tree, path)["oid"],
            by_path(&expected, path)["oid"],
            "{path}"
        );
        assert_eq!(
            by_path(&tree, path)["size"],
            by_path(&expected, path)["size"],
            "{path}"
        );
    }
    let big = by_path(&tree, "big.bin");
    assert_eq!(big["lfs"]["oid"], A_BIN_SHA256);
    assert_eq!(big["xetHash"], A_BIN_HASH);
    // The canonical pointer: 125 bytes and the size's digits, as the
    // fixture's 134 for a 9-digit size.
    let size = big["size"].as_u64().unwrap();
    assert_eq!(big["lfs"]["size"], size);
    assert_eq!(big["lfs"]["pointerSize"], 125 + size.to_string().len());

    // A folder goes with everything under it.
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("nested"),
            file("d/e/f.txt", b"f"),
            file("d/g.txt", b"g"),
        ],
    )
    .await;
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[
            header("drop d"),
            json!({"key": "deletedFolder", "value": {"path": "d"}}),
        ],
    )
    .await;
    assert!(!paths(&tree_of(&hub).await).contains(&"d".to_owned()));
    // Deleting what isn't there names it.
    let missing = hub
        .commit(
            ALICE,
            "/api/models/alice/m",
            "main",
            &[
                header("nothing"),
                json!({"key": "deletedFile", "value": {"path": "nope.txt"}}),
            ],
        )
        .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND, "{}", missing.text());
    assert_eq!(missing.header("x-error-code"), Some("EntryNotFound"));
    // Bob may read, not commit.
    let bob = hub
        .commit(
            BOB,
            "/api/models/alice/m",
            "main",
            &[header("x"), file("x", b"x")],
        )
        .await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN);
}

async fn tree_of(hub: &TestHub) -> Value {
    tree(hub, "main").await
}

#[tokio::test]
async fn commit_rejects_lfs_file_registered_to_another_scope() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "a", None).await;
    hub.create(ALICE, "alice", "b", None).await;
    let router = with_cas(&hub);
    upload_capture(&hub, &router, ALICE, "alice/a").await;
    let size = a_bin_size(&hub, &repo_id(&hub, "alice", "a").await).await;
    let lines = [header("dedup"), lfs_file("big.bin", A_BIN_SHA256, size)];
    // Uploaded to alice/a only: a commit to alice/b naming it fails with a
    // hint, never silently referencing alice/a's file (Review Focus 1).
    let refused = hub
        .commit(ALICE, "/api/models/alice/b", "main", &lines)
        .await;
    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "{}",
        refused.text()
    );
    let body = refused.json();
    assert_eq!(body["code"], "bad_request");
    assert!(
        body["hint"]
            .as_str()
            .unwrap()
            .contains("Upload the file to this repository first"),
        "{body}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("big.bin"),
        "{body}"
    );
    // The wrong size for a registered SHA-256 is refused too.
    let wrong = hub
        .commit(
            ALICE,
            "/api/models/alice/a",
            "main",
            &[header("dedup"), lfs_file("big.bin", A_BIN_SHA256, size + 1)],
        )
        .await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    // Once a shard registers it to alice/b too, as `hf upload` to alice/b
    // does with only a shard, the commit lands.
    upload_capture(&hub, &router, ALICE, "alice/b").await;
    commit_ok(&hub, "alice/b", "main", &lines).await;
    commit_ok(&hub, "alice/a", "main", &lines).await;
}

#[tokio::test]
async fn commit_parent_mismatch_412() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let initial = main_tip(&refs(&hub).await);
    let with_parent = |parent: &str, path: &str| {
        vec![
            json!({"key": "header", "value": {"summary": "s", "description": "", "parentCommit": parent}}),
            file(path, b"x"),
        ]
    };
    commit_ok(&hub, "alice/m", "main", &with_parent(&initial, "a.txt")).await;
    let stale = hub
        .commit(
            ALICE,
            "/api/models/alice/m",
            "main",
            &with_parent(&initial, "b.txt"),
        )
        .await;
    assert_eq!(
        stale.status,
        StatusCode::PRECONDITION_FAILED,
        "{}",
        stale.text()
    );
    assert_eq!(stale.json()["code"], "precondition_failed");
    let tip = main_tip(&refs(&hub).await);
    commit_ok(&hub, "alice/m", "main", &with_parent(&tip, "b.txt")).await;
}

#[tokio::test]
async fn commit_limits_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(LocalBucket::open(&dir.path().join("bucket")).unwrap());
    let cfg = HubConfig {
        lfs_threshold: HubConfig::MIN_LFS_THRESHOLD,
        max_commit_bytes: 4 << 20,
        ..TestHub::config()
    };
    let hub = TestHub::start_config(
        dir,
        bucket,
        support::users(),
        Arc::new(RulesAuthorizer::new(support::rules())),
        cfg,
    )
    .await;
    hub.create(ALICE, "alice", "m", None).await;
    // A regular file of at most `hub.lfs_threshold` travels in the commit.
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[header("fits"), file("a", &[1; 1024])],
    )
    .await;
    let large = hub
        .commit(
            ALICE,
            "/api/models/alice/m",
            "main",
            &[header("big"), file("b", &[1; 1025])],
        )
        .await;
    assert_eq!(large.status, StatusCode::BAD_REQUEST, "{}", large.text());
    assert!(large.json()["hint"].as_str().unwrap().contains("hf_xet"));
    // The body is at most `hub.max_commit_bytes`.
    let body = format!(
        "{{\"key\": \"header\", \"value\": {{\"summary\": \"{}\"}}}}\n",
        "s".repeat(4 << 20)
    );
    let too_big = hub
        .call_raw(
            Method::POST,
            "/api/models/alice/m/commit/main",
            Some(ALICE),
            "application/x-ndjson",
            body,
        )
        .await;
    assert_eq!(too_big.status, StatusCode::PAYLOAD_TOO_LARGE);
    // At most 25,000 operations.
    let delete = "{\"key\":\"deletedFile\",\"value\":{\"path\":\"a\"}}\n";
    let body = format!(
        "{{\"key\":\"header\",\"value\":{{\"summary\":\"s\"}}}}\n{}",
        delete.repeat(MAX_COMMIT_OPS + 1)
    );
    let many = hub
        .call_raw(
            Method::POST,
            "/api/models/alice/m/commit/main",
            Some(ALICE),
            "application/x-ndjson",
            body,
        )
        .await;
    assert_eq!(many.status, StatusCode::BAD_REQUEST);
    assert!(
        many.text().contains("more than 25000 operations"),
        "{}",
        many.text()
    );
}

#[tokio::test]
async fn commit_ndjson_accepts_python_separators() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    // The fixture's exact body: Python's `", "` and `": "` separators.
    let text =
        fixture("10-create-commit-delete--02-POST-models-commit.json")["request"]["body"]["text"]
            .as_str()
            .unwrap()
            .replace(
                "{\"key\": \"deletedFile\", \"value\": {\"path\": \"sub/small.txt\"}}\n",
                "",
            );
    let reply = hub
        .call_raw(
            Method::POST,
            "/api/models/alice/m/commit/main",
            Some(ALICE),
            "application/x-ndjson",
            text,
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    // Non-ASCII is `\u`-escaped by `json.dumps`, a surrogate pair beyond
    // the BMP; the path arrives as written.
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[header("utf-8 ü"), file("dir/ü 🦀.txt", b"crab")],
    )
    .await;
    let tree = tree(&hub, "main/dir").await;
    assert_eq!(paths(&tree), ["dir/ü 🦀.txt"]);
    let log = hub
        .call(
            Method::GET,
            "/api/models/alice/m/commits/main",
            Some(ALICE),
            None,
        )
        .await
        .json();
    assert_eq!(log[0]["title"], "utf-8 ü");
}

#[tokio::test]
async fn readme_commit_posts_validate_yaml_first() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    // `upload_folder` of a model card posts it here first and fails on any
    // non-2xx (`hf_api.py` lines 11469–11483).
    let name = "09-upload-folder--01-POST-validate-yaml.json";
    let reply = hub
        .call(
            Method::POST,
            "/api/validate-yaml",
            Some(ALICE),
            Some(&fixture(name)["request"]["body"]["json"]),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_read_keys(&fixture_json(name), &reply.json(), &["errors", "warnings"]);
    commit_ok(
        &hub,
        "alice/m",
        "main",
        &[header("card"), file("README.md", b"# openxet compat S3\n")],
    )
    .await;
}

#[tokio::test]
async fn branch_and_tag_shapes() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let call = |method: Method, path: &str, body: Option<Value>| {
        let hub = &hub;
        let path = format!("/api/models/alice/m/{path}");
        async move { hub.call(method, &path, Some(ALICE), body.as_ref()).await }
    };
    let created = call(Method::POST, "branch/s3-branch", Some(json!({}))).await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    assert_eq!(
        created.json(),
        fixture_json("11-create-branch--01-POST-models-branch.json")
    );
    // An existing branch is a 409 naming the ref, which
    // `create_branch(exist_ok=True)` swallows (S3 fixture `12`).
    let conflict = call(Method::POST, "branch/s3-branch", Some(json!({}))).await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    let text = "Reference refs/heads/s3-branch already exists";
    assert_eq!(conflict.header("x-error-message"), Some(text));
    assert_read_keys(
        &fixture_json("12-create-branch-conflict--01-POST-models-branch.json"),
        &conflict.json(),
        &["error"],
    );
    assert_eq!(conflict.json()["error"], text);
    // `startingPoint` names any revision.
    let initial = main_tip(&refs(&hub).await);
    commit_ok(&hub, "alice/m", "main", &[header("2"), file("x", b"x")]).await;
    let from = call(
        Method::POST,
        "branch/from-initial",
        Some(json!({"startingPoint": initial})),
    )
    .await;
    assert_eq!(from.status, StatusCode::OK, "{}", from.text());
    let listed = refs(&hub).await;
    let target = |name: &str| {
        listed["branches"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["name"] == name)
            .unwrap()["targetCommit"]
            .clone()
    };
    assert_eq!(target("from-initial"), json!(initial));
    // A starting point that names nothing is the revision answer.
    let nowhere = call(
        Method::POST,
        "branch/x",
        Some(json!({"startingPoint": "no-such-rev"})),
    )
    .await;
    assert_eq!(nowhere.status, StatusCode::NOT_FOUND);
    assert_eq!(nowhere.header("x-error-code"), Some("RevisionNotFound"));

    let request =
        fixture("13-create-tag--01-POST-models-tag.json")["request"]["body"]["json"].clone();
    let tagged = call(Method::POST, "tag/main", Some(request.clone())).await;
    assert_eq!(tagged.status, StatusCode::OK, "{}", tagged.text());
    assert_eq!(
        tagged.json(),
        fixture_json("13-create-tag--01-POST-models-tag.json")
    );
    assert_eq!(
        call(Method::POST, "tag/main", Some(request)).await.status,
        StatusCode::CONFLICT
    );
    let untagged = call(Method::DELETE, "tag/v0-s3", None).await;
    assert_eq!(untagged.status, StatusCode::OK, "{}", untagged.text());
    assert_eq!(
        untagged.json(),
        fixture_json("30-delete-tag--01-DELETE-models-tag.json")
    );
    let deleted = call(Method::DELETE, "branch/s3-branch", None).await;
    assert_eq!(deleted.status, StatusCode::OK);
    assert_eq!(
        deleted.json(),
        fixture_json("31-delete-branch--01-DELETE-models-branch.json")
    );
    let gone = call(Method::DELETE, "branch/s3-branch", None).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    assert_eq!(gone.header("x-error-code"), Some("RevisionNotFound"));
    // `main` stays.
    let main = call(Method::DELETE, "branch/main", None).await;
    assert_eq!(main.status, StatusCode::BAD_REQUEST, "{}", main.text());
    // Bob may read, not write refs.
    let bob = hub
        .call(
            Method::POST,
            "/api/models/alice/m/branch/b",
            Some(BOB),
            Some(&json!({})),
        )
        .await;
    assert_eq!(bob.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn tag_target_is_the_annotated_tag_object() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let tagged = hub
        .call(
            Method::POST,
            "/api/models/alice/m/tag/main",
            Some(ALICE),
            Some(&json!({"tag": "v0-s3", "message": "S3 tag"})),
        )
        .await;
    assert_eq!(tagged.status, StatusCode::OK, "{}", tagged.text());
    let listed = refs(&hub).await;
    assert_read_keys(
        &fixture_json("14-list-repo-refs--01-GET-models-refs.json"),
        &listed,
        &[
            "tags[].name",
            "tags[].ref",
            "tags[].targetCommit",
            "branches[].name",
            "branches[].ref",
            "branches[].targetCommit",
            "converts",
        ],
    );
    let tag = &listed["tags"][0];
    assert_eq!(
        (tag["name"].clone(), tag["ref"].clone()),
        (json!("v0-s3"), json!("refs/tags/v0-s3"))
    );
    // As on huggingface.co, the tag's target is the annotated tag object,
    // not the commit it tags (S3 fixture `14`).
    let target = tag["targetCommit"].as_str().unwrap();
    assert_ne!(target, main_tip(&listed));
    let id: openxet_index::RepoId = repo_id(&hub, "alice", "m").await.parse().unwrap();
    let snap = hub.state.engine.snapshot(id).await.unwrap();
    let git = |args: &[&str]| {
        let out = Command::new(hub.state.engine.git())
            .arg("--git-dir")
            .arg(snap.dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(git(&["cat-file", "-t", target]), "tag");
    assert_eq!(
        git(&["rev-parse", &format!("{target}^{{commit}}")]),
        main_tip(&listed)
    );
    assert!(
        git(&["cat-file", "-p", target]).contains("tagger alice <alice@users.noreply.openxet.com>")
    );
    // The tag resolves as a revision, to the commit.
    let tree = hub
        .call(
            Method::GET,
            "/api/models/alice/m/tree/v0-s3",
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(tree.status, StatusCode::OK);
    // With `include_prs`, `pullRequests` is there too (S3 fixture `15`).
    let with_prs = hub
        .call(
            Method::GET,
            "/api/models/alice/m/refs?include_prs=1",
            Some(ALICE),
            None,
        )
        .await
        .json();
    assert_eq!(with_prs["pullRequests"], json!([]));
    assert!(listed.get("pullRequests").is_none());
    // Python's `False`, as a client may format the flag, is off.
    let without = hub
        .call(
            Method::GET,
            "/api/models/alice/m/refs?include_prs=False",
            Some(ALICE),
            None,
        )
        .await
        .json();
    assert!(without.get("pullRequests").is_none());
}

/// Creating a repository writes huggingface.co's `initial commit` on
/// `main` (ruling M2-R5): by openxet, with no parent, holding only the
/// default `.gitattributes`.
#[tokio::test]
async fn create_writes_initial_commit() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let id: openxet_index::RepoId = repo_id(&hub, "alice", "m").await.parse().unwrap();
    let snap = hub.state.engine.snapshot(id).await.unwrap();
    let git = |args: &[&str]| {
        let out = Command::new(hub.state.engine.git())
            .arg("--git-dir")
            .arg(snap.dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    };
    assert_eq!(
        git(&["for-each-ref", "--format=%(refname)"]),
        "refs/heads/main"
    );
    let commit = git(&["cat-file", "-p", "refs/heads/main"]);
    let headers: Vec<&str> = commit.lines().take_while(|l| !l.is_empty()).collect();
    assert!(
        !headers.iter().any(|l| l.starts_with("parent ")),
        "{commit}"
    );
    assert!(
        headers
            .iter()
            .any(|l| l.starts_with("author openxet <noreply@openxet.com> ")),
        "{commit}"
    );
    assert!(
        headers
            .iter()
            .any(|l| l.starts_with("committer openxet <noreply@openxet.com> ")),
        "{commit}"
    );
    assert!(commit.ends_with("\n\ninitial commit"), "{commit}");
    assert_eq!(
        git(&["ls-tree", "refs/heads/main"]),
        "100644 blob a6344aac8c09253b3b630fb776ae94478aa0275b\t.gitattributes"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_hub_commits_both_land() {
    let hub = Arc::new(hub().await);
    hub.create(ALICE, "alice", "m", None).await;
    // Two `create_commit`s to one branch at once, on two threads, released
    // together, neither naming a parent: both land, one after the other,
    // and neither is lost (Review Focus 2).
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let spawn = |name: &'static str| {
        let (hub, barrier) = (Arc::clone(&hub), Arc::clone(&barrier));
        tokio::spawn(async move {
            let lines = [header(name), file(&format!("{name}.txt"), name.as_bytes())];
            barrier.wait().await;
            hub.commit(ALICE, "/api/models/alice/m", "main", &lines)
                .await
        })
    };
    let (a, b) = (spawn("one"), spawn("two"));
    let (a, b) = (a.await.unwrap(), b.await.unwrap());
    assert_eq!(a.status, StatusCode::OK, "{}", a.text());
    assert_eq!(b.status, StatusCode::OK, "{}", b.text());
    assert_ne!(a.json()["commitOid"], b.json()["commitOid"]);
    let tree = tree(&hub, "main").await;
    assert_eq!(paths(&tree), [".gitattributes", "one.txt", "two.txt"]);
    let log = hub
        .call(
            Method::GET,
            "/api/models/alice/m/commits/main",
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(log.header("x-total-count"), Some("3"));
}
