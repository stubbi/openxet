//! Repository create and delete on the Hub API, against S3's huggingface.co
//! fixtures (`docs/research/S3-hub-api-shapes.md`), and the name rules of
//! ruling M2-R2.

#![allow(clippy::unwrap_used)]

use axum::http::{Method, StatusCode};
use openxet_auth::Action;
use openxet_hub::test_support::{
    ALL, PUBLIC_URL, TestHub, assert_read_keys, fixture, fixture_json, rule, user_token,
};
use serde_json::{Value, json};

const ALICE: &str = "oxt_test_alice";
const ADMIN: &str = "oxt_test_admin";

/// Alice may do everything in `alice/*`; the admin everywhere.
async fn hub() -> TestHub {
    TestHub::start(
        vec![
            user_token(ALICE, "alice", &[]),
            user_token(ADMIN, "admin", &[]),
        ],
        vec![
            rule(&["alice"], &["alice/*"], ALL),
            rule(&["admin"], &["*"], ALL),
        ],
    )
    .await
}

async fn create(hub: &TestHub, token: &str, body: &Value) -> openxet_hub::test_support::Reply {
    hub.call(Method::POST, "/api/repos/create", Some(token), Some(body))
        .await
}

#[tokio::test]
async fn create_answers_s3_shape() {
    let hub = hub().await;
    // The request `create_repo` sends, with the namespace and name swapped.
    let mut request =
        fixture("04-create-model--01-POST-repos-create.json")["request"]["body"]["json"].clone();
    request["organization"] = "alice".into();
    request["name"] = "m".into();
    let reply = create(&hub, ALICE, &request).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    // `create_repo` reads only `url` (hf_api.py line 4723); the rest is the
    // observed shape.
    assert_read_keys(
        &fixture_json("04-create-model--01-POST-repos-create.json"),
        &body,
        &["url", "name", "id", "filesCopyPending"],
    );
    assert_eq!(body["url"], format!("{PUBLIC_URL}/alice/m"));
    assert_eq!(body["name"], "alice/m");
    assert_eq!(body["id"].as_str().unwrap().len(), 32);

    let reply = create(
        &hub,
        ALICE,
        &json!({"name": "d", "organization": "alice", "type": "dataset"}),
    )
    .await;
    assert_eq!(
        reply.json()["url"],
        format!("{PUBLIC_URL}/datasets/alice/d")
    );

    let info = hub
        .call(Method::GET, "/api/models/alice/m", Some(ALICE), None)
        .await;
    assert_eq!(info.status, StatusCode::OK);
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
            "gated",
            "disabled",
            "config",
            "spaces",
            "createdAt",
        ],
    );
    assert_eq!(info["id"], "alice/m");
    assert_eq!(info["private"], true);
    assert_eq!(info["labels"], json!(["model"]));
    // `%Y-%m-%dT%H:%M:%S.%fZ`, the one form `parse_datetime` reads.
    let created = info["createdAt"].as_str().unwrap();
    assert_eq!(created.len(), "2026-10-23T05:00:48.000Z".len(), "{created}");
    assert!(created.ends_with('Z') && created.as_bytes()[19] == b'.');

    let dataset = hub
        .call(Method::GET, "/api/datasets/alice/d", Some(ALICE), None)
        .await
        .json();
    assert_read_keys(
        &fixture_json("42-dataset-info--01-GET-datasets.json"),
        &dataset,
        &["_id", "id", "private", "author", "createdAt"],
    );
    assert!(dataset.get("modelId").is_none(), "{dataset}");
}

#[tokio::test]
async fn create_409_carries_url() {
    let hub = hub().await;
    let body = json!({"name": "m", "organization": "alice", "type": "model"});
    assert_eq!(create(&hub, ALICE, &body).await.status, StatusCode::OK);
    for name in [
        "05-create-model-exists-conflict--01-POST-repos-create.json",
        "06-create-model-exist-ok--01-POST-repos-create.json",
    ] {
        let reply = create(&hub, ALICE, &body).await;
        assert_eq!(reply.status, StatusCode::CONFLICT);
        let json = reply.json();
        // `create_repo(exist_ok=True)` reads `url` from the 409 (S3 answer 5).
        assert_read_keys(&fixture_json(name), &json, &["error", "url"]);
        assert_eq!(json["url"], format!("{PUBLIC_URL}/alice/m"));
        assert_eq!(json["code"], "name_taken");
        assert_eq!(
            reply.header("x-error-message").unwrap(),
            json["error"].as_str().unwrap()
        );
        assert!(reply.header("x-error-code").is_none());
    }
    // Names are unique across labels: the dataset view of the same name
    // conflicts, with that view's URL and, for a reader, the label hint.
    let reply = create(
        &hub,
        ALICE,
        &json!({"name": "M", "organization": "alice", "type": "dataset"}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    let json = reply.json();
    assert_eq!(json["url"], format!("{PUBLIC_URL}/datasets/alice/M"));
    let error = json["error"].as_str().unwrap();
    assert!(error.contains("[model]"), "{error}");
    assert!(
        error.contains("openxet label add alice/m dataset"),
        "{error}"
    );
}

#[tokio::test]
async fn create_in_foreign_namespace_403_names_namespace() {
    let hub = hub().await;
    let expected = fixture_json("08-create-foreign-namespace-cli--01-POST-repos-create.json");
    let reply = create(
        &hub,
        ALICE,
        &json!({"name": "m", "organization": "huggingface", "visibility": "private", "type": "model"}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    let json = reply.json();
    // The text of S3 lesson 12, which `hf` shows from `X-Error-Message`.
    assert_eq!(json["error"], expected["error"]);
    assert_eq!(
        reply.header("x-error-message").unwrap(),
        expected["error"].as_str().unwrap()
    );
    assert!(json["hint"].as_str().unwrap().contains("[[auth.rules]]"));

    let reply = create(
        &hub,
        ALICE,
        &json!({"name": "d", "organization": "huggingface", "type": "dataset"}),
    )
    .await;
    assert_eq!(
        reply.json()["error"],
        "You don't have the rights to create a dataset under the namespace \"huggingface\""
    );
}

#[tokio::test]
async fn delete_is_text_ok_and_404_shape() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", Some("model")).await;
    hub.create(ALICE, "alice", "d", Some("dataset")).await;
    for (fixture_name, body) in [
        (
            "91-delete-dataset--01-DELETE-repos-delete.json",
            json!({"name": "d", "organization": "alice", "type": "dataset"}),
        ),
        (
            "92-delete-model--01-DELETE-repos-delete.json",
            json!({"name": "m", "organization": "alice", "type": "model"}),
        ),
    ] {
        let expected = &fixture(fixture_name)["response"];
        let reply = hub
            .call(
                Method::DELETE,
                "/api/repos/delete",
                Some(ALICE),
                Some(&body),
            )
            .await;
        assert_eq!(reply.status.as_u16(), expected["status"]);
        assert_eq!(reply.text(), expected["body"]["text"].as_str().unwrap());
        assert_eq!(
            reply.header("content-type"),
            Some("text/plain; charset=utf-8")
        );
    }
    let info = hub
        .call(Method::GET, "/api/models/alice/m", Some(ALICE), None)
        .await;
    assert_eq!(info.status, StatusCode::NOT_FOUND);

    let expected = &fixture("93-delete-model-missing--01-DELETE-repos-delete.json")["response"];
    let reply = hub
        .call(
            Method::DELETE,
            "/api/repos/delete",
            Some(ALICE),
            Some(&json!({"name": "m", "organization": "alice", "type": "model"})),
        )
        .await;
    assert_eq!(reply.status.as_u16(), expected["status"]);
    assert_eq!(reply.json()["error"], expected["body"]["json"]["error"]);
    assert_eq!(reply.header("x-error-code"), Some("RepoNotFound"));
    assert_eq!(
        reply.header("x-error-message"),
        Some("Repository not found")
    );

    // The name is free again.
    hub.create(ALICE, "alice", "m", None).await;
}

#[tokio::test]
async fn untyped_create_adds_model_label() {
    let hub = hub().await;
    // `create_repo` with no `repo_type` sends no `type` (fixture `07`).
    let request = fixture("07-create-foreign-namespace--01-POST-repos-create.json");
    assert!(request["request"]["body"]["json"].get("type").is_none());
    let reply = create(
        &hub,
        ALICE,
        &json!({"name": "plain", "organization": "alice"}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["url"], format!("{PUBLIC_URL}/alice/plain"));
    let info = hub
        .call(Method::GET, "/api/models/alice/plain", Some(ALICE), None)
        .await;
    assert_eq!(info.status, StatusCode::OK);
    assert_eq!(info.json()["labels"], json!(["model"]));
    // Without `organization`, the namespace is the caller's own name, as
    // `whoami-v2` answers it.
    let reply = create(&hub, ALICE, &json!({"name": "mine"})).await;
    assert_eq!(reply.json()["name"], "alice/mine");
}

#[tokio::test]
async fn names_validated() {
    let hub = hub().await;
    let long = "a".repeat(97);
    let cases: &[(&str, &str, &str)] = &[
        ("", "m", "^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$"),
        ("ns", "", "^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$"),
        ("ns", &long, "^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$"),
        ("-ns", "m", "^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$"),
        ("ns", ".m", "^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$"),
        ("ns", "m/x", "^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$"),
        ("ns", "caf\u{e9}", "^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$"),
        ("ns", "a..b", "`..`"),
        ("n..s", "m", "`..`"),
        ("ns", "m.git", "`.git`"),
        ("ns", "m.GIT", "`.git`"),
        ("models", "m", "route prefixes"),
        ("API", "m", "route prefixes"),
        ("openapi.json", "m", "route prefixes"),
    ];
    for (namespace, name, rule) in cases {
        let reply = create(
            &hub,
            ADMIN,
            &json!({"name": name, "organization": namespace}),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{namespace}/{name}");
        let json = reply.json();
        assert_eq!(json["code"], "invalid_name", "{namespace}/{name}");
        let hint = json["hint"].as_str().unwrap();
        assert!(hint.contains(rule), "{namespace}/{name}: {hint}");
    }
    // The longest name, and every allowed character.
    let longest = "a".repeat(96);
    for (namespace, name) in [("ns", longest.as_str()), ("N.s_-9", "A1._-z")] {
        let reply = create(
            &hub,
            ADMIN,
            &json!({"name": name, "organization": namespace}),
        )
        .await;
        assert_eq!(
            reply.status,
            StatusCode::OK,
            "{namespace}/{name}: {}",
            reply.text()
        );
    }
}

#[tokio::test]
async fn name_taken_case_insensitively_409() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "Model", None).await;
    let reply = create(
        &hub,
        ALICE,
        &json!({"name": "mODEL", "organization": "ALICE"}),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json()["url"], format!("{PUBLIC_URL}/ALICE/mODEL"));
    // Stored as written: every spelling finds it, and it answers its own.
    let info = hub
        .call(Method::GET, "/api/models/alice/model", Some(ALICE), None)
        .await;
    assert_eq!(info.json()["id"], "alice/Model");
}

#[tokio::test]
async fn create_needs_a_well_formed_body() {
    let hub = hub().await;
    for (body, needle) in [
        (json!({"organization": "alice"}), "missing field `name`"),
        (
            json!({"name": "m", "organization": "alice", "type": "bucket"}),
            "unknown repository type",
        ),
        (
            json!({"name": "m", "organization": "alice", "visibility": "hidden"}),
            "unknown visibility",
        ),
        (json!({"name": 7, "organization": "alice"}), "line 1"),
    ] {
        let reply = create(&hub, ALICE, &body).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        let message = reply.json()["message"].as_str().unwrap().to_owned();
        assert!(message.contains(needle), "{body}: {message}");
    }
    // Only the admin may delete outside `alice/*`; a reader of a repository
    // who can't write it is refused by name.
    let reader = TestHub::start(
        vec![
            user_token(ALICE, "alice", &[]),
            user_token(ADMIN, "admin", &[]),
        ],
        vec![
            rule(&["alice"], &["*"], &[Action::Read]),
            rule(&["admin"], &["*"], ALL),
        ],
    )
    .await;
    reader.create(ADMIN, "org", "m", None).await;
    let reply = reader
        .call(
            Method::DELETE,
            "/api/repos/delete",
            Some(ALICE),
            Some(&json!({"name": "m", "organization": "org"})),
        )
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        reply.json()["error"],
        "You don't have the rights to delete org/m"
    );
}
