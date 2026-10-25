//! The labels API (ruling M2-R12, ADR 0010 item 3).

#![allow(clippy::unwrap_used)]

mod common;

use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use common::{HookAuthz, HookBucket};
use openxet_auth::{Action, RulesAuthorizer};
use openxet_hub::test_support::{ALL, TestHub, rule, user_token};
use openxet_index::{Repos, SqliteIndex};
use openxet_storage::{Bucket, RepoMeta};
use serde_json::{Value, json};

const ALICE: &str = "oxt_test_alice";
const READER: &str = "oxt_test_reader";
const LABELS: &str = "/api/openxet/v1/repos/alice/m/labels";

async fn hub() -> TestHub {
    let hub = TestHub::start(
        vec![
            user_token(ALICE, "alice", &[]),
            user_token(READER, "reader", &[]),
        ],
        vec![
            rule(&["alice"], &["alice/*"], ALL),
            rule(&["reader"], &["alice/*"], &[Action::Read]),
        ],
    )
    .await;
    hub.create(ALICE, "alice", "m", None).await;
    hub
}

async fn labels(hub: &TestHub, method: Method, path: &str, token: &str) -> (StatusCode, Value) {
    let reply = hub.call(method, path, Some(token), None).await;
    (reply.status, reply.json())
}

#[tokio::test]
async fn label_add_remove_list_round_trip() {
    let hub = hub().await;
    let (status, body) = labels(&hub, Method::GET, LABELS, READER).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!({"schema": "openxet.labels.v1", "repo": "alice/m", "labels": ["model"]})
    );
    let (status, body) = labels(&hub, Method::PUT, &format!("{LABELS}/eval-logs"), ALICE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["labels"], json!(["model", "eval-logs"]));
    // Adding a label it carries, or removing one it doesn't, changes nothing.
    let (_, body) = labels(&hub, Method::PUT, &format!("{LABELS}/eval-logs"), ALICE).await;
    assert_eq!(body["labels"], json!(["model", "eval-logs"]));
    let (_, body) = labels(&hub, Method::DELETE, &format!("{LABELS}/model"), ALICE).await;
    assert_eq!(body["labels"], json!(["eval-logs"]));
    let (_, body) = labels(&hub, Method::DELETE, &format!("{LABELS}/model"), ALICE).await;
    assert_eq!(body["labels"], json!(["eval-logs"]));
    let (_, body) = labels(&hub, Method::GET, LABELS, ALICE).await;
    assert_eq!(body["labels"], json!(["eval-logs"]));

    // Without the `model` label the model view no longer sees it; the
    // labels API, openxet's own address, still does.
    let reply = hub
        .call(Method::GET, "/api/models/alice/m", Some(ALICE), None)
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    // A reader may list but not change labels.
    let reply = hub
        .call(Method::PUT, &format!("{LABELS}/x"), Some(READER), None)
        .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(
        reply.json()["error"],
        "You don't have the rights to change the labels of alice/m"
    );
}

#[tokio::test]
async fn label_rules_enforced() {
    let hub = hub().await;
    let long = "a".repeat(65);
    for (label, rule) in [
        ("Model", "lowercase slug"),
        ("a_b", "lowercase slug"),
        ("caf%C3%A9", "lowercase slug"),
        (long.as_str(), "1 to 64 bytes"),
    ] {
        let reply = hub
            .call(Method::PUT, &format!("{LABELS}/{label}"), Some(ALICE), None)
            .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{label}");
        let json = reply.json();
        assert_eq!(json["code"], "invalid_label");
        assert!(
            json["hint"].as_str().unwrap().contains(rule),
            "{label}: {json}"
        );
    }
    let longest = "a".repeat(64);
    let (status, _) = labels(&hub, Method::PUT, &format!("{LABELS}/{longest}"), ALICE).await;
    assert_eq!(status, StatusCode::OK);
    // `model` and the 64-byte one make two; 30 more fill the 32.
    for i in 0..30 {
        let (status, _) = labels(&hub, Method::PUT, &format!("{LABELS}/l{i}"), ALICE).await;
        assert_eq!(status, StatusCode::OK);
    }
    let reply = hub
        .call(
            Method::PUT,
            &format!("{LABELS}/one-more"),
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert!(
        reply.json()["hint"]
            .as_str()
            .unwrap()
            .contains("at most 32")
    );
}

/// A second handle on the Hub's index database, to change or read rows
/// while a request runs, as another node would.
async fn observer(hub: &TestHub) -> Arc<SqliteIndex> {
    Arc::new(SqliteIndex::open(&hub.index_path()).await.unwrap())
}

async fn hooked() -> (TestHub, Arc<HookBucket>, Arc<HookAuthz>) {
    let dir = tempfile::tempdir().unwrap();
    let bucket = HookBucket::open(&dir.path().join("bucket"));
    let authz = HookAuthz::new(RulesAuthorizer::new(vec![rule(
        &["alice"],
        &["alice/*"],
        ALL,
    )]));
    let hub = TestHub::start_custom(
        dir,
        Arc::clone(&bucket) as Arc<dyn Bucket>,
        vec![user_token(ALICE, "alice", &[])],
        Arc::clone(&authz) as Arc<dyn openxet_auth::Authorizer>,
    )
    .await;
    hub.create(ALICE, "alice", "m", None).await;
    (hub, bucket, authz)
}

fn strings(labels: &[&str]) -> Vec<String> {
    labels.iter().map(|&l| l.to_owned()).collect()
}

#[tokio::test]
async fn label_change_reaches_bucket_before_index_and_writes_audit() {
    let (hub, bucket, _) = hooked().await;
    let index = observer(&hub).await;
    // At each metadata write, the labels the index row holds at that moment.
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (hook_index, hook_seen) = (Arc::clone(&index), Arc::clone(&seen));
    bucket.set_hook(Arc::new(move |meta: RepoMeta| {
        let (index, seen) = (Arc::clone(&hook_index), Arc::clone(&hook_seen));
        Box::pin(async move {
            let row = index
                .repo_by_name(&meta.namespace, &meta.name)
                .await
                .unwrap()
                .unwrap();
            seen.lock().unwrap().push((meta.labels, row.labels));
        })
    }));

    labels(&hub, Method::PUT, &format!("{LABELS}/dataset"), ALICE).await;
    labels(&hub, Method::DELETE, &format!("{LABELS}/model"), ALICE).await;
    // Each record reached the bucket while the index still held the old
    // labels: bucket first, then index (design §5.3, ruling M2-R12).
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (strings(&["model", "dataset"]), strings(&["model"])),
            (strings(&["dataset"]), strings(&["model", "dataset"])),
        ]
    );
    // The bucket's record is what the index now says.
    let repo = index.repo_by_name("alice", "m").await.unwrap().unwrap();
    assert_eq!(bucket.meta(&repo.id.to_string()).await.labels, repo.labels);

    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", hub.index_path().display()))
        .await
        .unwrap();
    let rows: Vec<(String, String, Vec<u8>, String)> =
        sqlx::query_as("SELECT actor, action, repo, detail FROM audit_log ORDER BY seq")
            .fetch_all(&pool)
            .await
            .unwrap();
    let actions: Vec<(&str, &str, Value)> = rows
        .iter()
        .map(|(actor, action, _, detail)| {
            (
                actor.as_str(),
                action.as_str(),
                serde_json::from_str(detail).unwrap(),
            )
        })
        .collect();
    assert_eq!(actions[0].1, "repo.create");
    assert_eq!(
        actions[1..],
        [
            ("alice", "label.add", json!({"label": "dataset"})),
            ("alice", "label.remove", json!({"label": "model"})),
        ]
    );
    assert!(rows.iter().all(|(_, _, id, _)| id[..] == repo.id.0[..]));
}

/// A change whose labels went stale after the request read them, before
/// it wrote anything, answers `409` and writes nothing to the bucket
/// (ruling M2-R12).
#[tokio::test]
async fn stale_label_list_is_409_and_writes_nothing() {
    let (hub, bucket, authz) = hooked().await;
    let index = observer(&hub).await;
    let id = index.repo_by_name("alice", "m").await.unwrap().unwrap().id;
    // Another writer adds `dataset` between the read and the write check.
    authz.set_hook(Arc::new(move || {
        let index = Arc::clone(&index);
        Box::pin(async move {
            let _ = index
                .set_labels(id, &strings(&["model"]), &strings(&["model", "dataset"]))
                .await;
        })
    }));
    let writes = bucket.replaced().len();
    let reply = hub
        .call(Method::PUT, &format!("{LABELS}/space"), Some(ALICE), None)
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    let json = reply.json();
    assert_eq!(json["code"], "labels_changed");
    assert_eq!(
        json["hint"],
        "Labels changed concurrently; re-read and retry."
    );
    assert_eq!(bucket.replaced().len(), writes, "the bucket was written");
    // A retry reads the new list and succeeds.
    let (status, body) = labels(&hub, Method::PUT, &format!("{LABELS}/space"), ALICE).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["labels"], json!(["model", "dataset", "space"]));
}

/// A change that loses the index's compare-and-swap after its bucket write
/// answers `409`, and the bucket keeps the index's labels, not the refused
/// change.
#[tokio::test]
async fn lost_label_swap_is_409_and_the_bucket_matches_the_index() {
    let (hub, bucket, _) = hooked().await;
    let index = observer(&hub).await;
    let id = index.repo_by_name("alice", "m").await.unwrap().unwrap().id;
    // The concurrent change lands right after this request's bucket write.
    let fired = Arc::new(Mutex::new(false));
    bucket.set_hook(Arc::new(move |meta: RepoMeta| {
        let (index, fired) = (Arc::clone(&index), Arc::clone(&fired));
        Box::pin(async move {
            if meta.labels.contains(&"space".to_owned()) && !*fired.lock().unwrap() {
                *fired.lock().unwrap() = true;
                index
                    .set_labels(id, &strings(&["model"]), &strings(&["model", "dataset"]))
                    .await
                    .unwrap();
            }
        })
    }));
    let reply = hub
        .call(Method::PUT, &format!("{LABELS}/space"), Some(ALICE), None)
        .await;
    assert_eq!(reply.status, StatusCode::CONFLICT);
    assert_eq!(reply.json()["code"], "labels_changed");
    let stored = bucket.meta(&id.to_string()).await;
    assert_eq!(stored.labels, strings(&["model", "dataset"]));
    assert_eq!(stored.deleted_at, None);
}
