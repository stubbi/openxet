//! The labels API (ruling M2-R12, ADR 0010 item 3).

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::http::{Method, StatusCode};
use openxet_auth::Action;
use openxet_hub::test_support::{ALL, TestHub, rule, user_token};
use openxet_index::{Repos, SqliteIndex};
use openxet_storage::{
    BodyStream, Bucket, Key, KeyKind, LocalBucket, RangeStream, RepoIdHex, RepoMeta, StorageError,
};
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

/// A bucket that records, at each metadata write, the labels the index
/// row held at that moment.
struct Recording {
    inner: LocalBucket,
    index: Mutex<Option<Arc<SqliteIndex>>>,
    seen: Mutex<Vec<(Vec<String>, Vec<String>)>>,
}

#[async_trait::async_trait]
impl Bucket for Recording {
    async fn put(&self, key: &Key, bytes: Bytes) -> Result<(), StorageError> {
        self.inner.put(key, bytes).await
    }
    async fn create(&self, key: &Key, bytes: Bytes) -> Result<bool, StorageError> {
        self.inner.create(key, bytes).await
    }
    async fn list(&self, kind: KeyKind) -> Result<Vec<Key>, StorageError> {
        self.inner.list(kind).await
    }
    async fn get_range(&self, key: &Key, start: u64, end: u64) -> Result<Bytes, StorageError> {
        self.inner.get_range(key, start, end).await
    }
    async fn get_range_stream(
        &self,
        key: &Key,
        start: u64,
        end: u64,
    ) -> Result<RangeStream, StorageError> {
        self.inner.get_range_stream(key, start, end).await
    }
    async fn size(&self, key: &Key) -> Result<Option<u64>, StorageError> {
        self.inner.size(key).await
    }
    async fn put_replace(&self, key: Key, bytes: Bytes) -> Result<(), StorageError> {
        let meta = RepoMeta::from_bytes(&key, &bytes).unwrap();
        let index = self.index.lock().unwrap().clone();
        if let Some(index) = index {
            let row = index
                .repo_by_name(&meta.namespace, &meta.name)
                .await
                .unwrap()
                .map(|r| r.labels)
                .unwrap_or_default();
            self.seen.lock().unwrap().push((meta.labels.clone(), row));
        }
        self.inner.put_replace(key, bytes).await
    }
    async fn put_stream(&self, key: Key, body: BodyStream, len: u64) -> Result<(), StorageError> {
        self.inner.put_stream(key, body, len).await
    }
    async fn get_to_file(&self, key: Key, path: &Path) -> Result<u64, StorageError> {
        self.inner.get_to_file(key, path).await
    }
    async fn last_modified(&self, key: Key) -> Result<Option<u64>, StorageError> {
        self.inner.last_modified(key).await
    }
    async fn list_under(&self, repo: RepoIdHex, kind: KeyKind) -> Result<Vec<Key>, StorageError> {
        self.inner.list_under(repo, kind).await
    }
}

#[tokio::test]
async fn label_change_reaches_bucket_before_index_and_writes_audit() {
    let dir = tempfile::tempdir().unwrap();
    let bucket = Arc::new(Recording {
        inner: LocalBucket::open(&dir.path().join("bucket")).unwrap(),
        index: Mutex::new(None),
        seen: Mutex::new(Vec::new()),
    });
    let hub = TestHub::start_with(
        dir,
        Arc::clone(&bucket) as Arc<dyn Bucket>,
        vec![user_token(ALICE, "alice", &[])],
        vec![rule(&["alice"], &["alice/*"], ALL)],
    )
    .await;
    hub.create(ALICE, "alice", "m", None).await;
    // A second handle on the same database, to read the row mid-write.
    let observer = Arc::new(SqliteIndex::open(&hub.index_path()).await.unwrap());
    *bucket.index.lock().unwrap() = Some(observer);

    labels(&hub, Method::PUT, &format!("{LABELS}/dataset"), ALICE).await;
    labels(&hub, Method::DELETE, &format!("{LABELS}/model"), ALICE).await;
    // Each record reached the bucket while the index still held the old
    // labels: bucket first, then index (design §5.3, ruling M2-R12).
    assert_eq!(
        *bucket.seen.lock().unwrap(),
        [
            (
                vec!["model".to_owned(), "dataset".to_owned()],
                vec!["model".to_owned()]
            ),
            (
                vec!["dataset".to_owned()],
                vec!["model".to_owned(), "dataset".to_owned()]
            ),
        ]
    );
    // The bucket's record is what the index now says.
    let repo = SqliteIndex::open(&hub.index_path())
        .await
        .unwrap()
        .repo_by_name("alice", "m")
        .await
        .unwrap()
        .unwrap();
    let key = Key::RepoMeta {
        repo: RepoIdHex::parse(&repo.id.to_string()).unwrap(),
    };
    let stored = RepoMeta::from_bytes(&key, &bucket.get(&key).await.unwrap().unwrap()).unwrap();
    assert_eq!(stored.labels, repo.labels);

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
