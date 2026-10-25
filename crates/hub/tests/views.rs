//! The typed views of ADR 0010 item 4: Hub routes see a repository through
//! one label.

#![allow(clippy::unwrap_used)]

use axum::http::{Method, StatusCode};
use openxet_auth::UserPrincipal;
use openxet_hub::test_support::{ALL, PUBLIC_URL, TestHub, rule, user_token};
use openxet_hub::{HubConfig, HubError, RepoAccess, View};
use serde_json::Value;

const ALICE: &str = "oxt_test_alice";

async fn hub() -> TestHub {
    TestHub::start(
        vec![user_token(ALICE, "alice", &[])],
        vec![rule(&["alice"], &["*"], ALL)],
    )
    .await
}

fn ids(listing: &Value) -> Vec<&str> {
    listing
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn typed_listing_filters_by_label() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "b-model", None).await;
    hub.create(ALICE, "alice", "a-data", Some("dataset")).await;
    hub.create(ALICE, "bob", "c-model", Some("model")).await;
    hub.create(ALICE, "alice", "d-space", Some("space")).await;

    let models = hub
        .call(Method::GET, "/api/models", Some(ALICE), None)
        .await;
    assert_eq!(models.status, StatusCode::OK);
    assert_eq!(ids(&models.json()), ["alice/b-model", "bob/c-model"]);
    assert!(models.header("link").is_none());
    let datasets = hub
        .call(Method::GET, "/api/datasets", Some(ALICE), None)
        .await;
    assert_eq!(ids(&datasets.json()), ["alice/a-data"]);
    assert!(datasets.json()[0].get("modelId").is_none());
    let spaces = hub
        .call(Method::GET, "/api/spaces", Some(ALICE), None)
        .await;
    assert_eq!(ids(&spaces.json()), ["alice/d-space"]);
    let kernels = hub
        .call(Method::GET, "/api/kernels", Some(ALICE), None)
        .await;
    assert_eq!(kernels.json(), serde_json::json!([]));

    let by_author = hub
        .call(Method::GET, "/api/models?author=bob", Some(ALICE), None)
        .await;
    assert_eq!(ids(&by_author.json()), ["bob/c-model"]);

    // Filters and sorts openxet doesn't do answer 501, never an unfiltered
    // listing.
    let search = hub
        .call(Method::GET, "/api/models?search=x", Some(ALICE), None)
        .await;
    assert_eq!(search.status, StatusCode::NOT_IMPLEMENTED);
    let full = hub
        .call(Method::GET, "/api/models?full=True", Some(ALICE), None)
        .await;
    assert_eq!(full.status, StatusCode::OK);
}

#[tokio::test]
async fn listings_paginate_and_list_only_what_the_caller_reads() {
    let hub = TestHub::start(
        vec![
            user_token(ALICE, "alice", &[]),
            user_token("oxt_test_reader", "reader", &[]),
        ],
        vec![
            rule(&["alice"], &["*"], ALL),
            rule(&["reader"], &["pub/*"], &[openxet_auth::Action::Read]),
        ],
    )
    .await;
    for name in ["a", "b", "c", "d", "e"] {
        hub.create(ALICE, "pub", name, None).await;
        hub.create(ALICE, "priv", name, None).await;
    }
    let mut seen = Vec::new();
    let mut next = "/api/models?limit=2".to_owned();
    loop {
        let page = hub
            .call(Method::GET, &next, Some("oxt_test_reader"), None)
            .await;
        assert_eq!(page.status, StatusCode::OK, "{}", page.text());
        let page_ids = ids(&page.json())
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>();
        assert!(page_ids.len() <= 2);
        seen.extend(page_ids);
        let Some(link) = page.header("link") else {
            break;
        };
        let url = link
            .strip_prefix('<')
            .and_then(|l| l.strip_suffix(">; rel=\"next\""))
            .unwrap();
        assert!(
            url.starts_with(&format!("{PUBLIC_URL}/api/models?")),
            "{url}"
        );
        assert!(url.contains("limit=2"), "{url}");
        next = url.strip_prefix(PUBLIC_URL).unwrap().to_owned();
    }
    assert_eq!(seen, ["pub/a", "pub/b", "pub/c", "pub/d", "pub/e"]);

    let bad = hub
        .call(Method::GET, "/api/models?cursor=%%%", Some(ALICE), None)
        .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn wrong_type_with_read_access_shows_label_hint() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "m", None).await;
    let reply = hub
        .call(Method::GET, "/api/datasets/alice/m", Some(ALICE), None)
        .await;
    // `RepoNotFound`, so stock clients raise `RepositoryNotFoundError`, with
    // the hint in the message and `X-Error-Message`, which they append.
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(reply.header("x-error-code"), Some("RepoNotFound"));
    let message = reply.header("x-error-message").unwrap();
    assert!(
        message.contains("alice/m exists without the `dataset` label"),
        "{message}"
    );
    assert!(
        message.contains("openxet label add alice/m dataset"),
        "{message}"
    );
    assert_eq!(reply.json()["error"], message);

    // Once labelled, the view serves it.
    let added = hub
        .call(
            Method::PUT,
            "/api/openxet/v1/repos/alice/m/labels/dataset",
            Some(ALICE),
            None,
        )
        .await;
    assert_eq!(added.status, StatusCode::OK);
    let reply = hub
        .call(Method::GET, "/api/datasets/alice/m", Some(ALICE), None)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
}

#[tokio::test]
async fn unprefixed_git_address_serves_any_label() {
    let hub = hub().await;
    hub.create(ALICE, "alice", "d", Some("dataset")).await;
    let alice = UserPrincipal {
        subject: "alice".to_owned(),
        name: "alice".to_owned(),
        email: None,
        claims: std::collections::BTreeMap::new(),
    };
    let access = RepoAccess::resolve(&hub.state, alice.clone(), "alice", "d", View::Any)
        .await
        .unwrap();
    assert_eq!(access.repo.labels, ["dataset"]);
    assert_eq!(access.view, View::Any);
    let model = RepoAccess::resolve(&hub.state, alice, "alice", "d", View::Model).await;
    assert!(
        matches!(model, Err(HubError::WrongView { label: "model", .. })),
        "{model:?}"
    );
    // The model view's URLs are unprefixed, the others' carry the type.
    let cfg: &HubConfig = &hub.state.cfg;
    assert_eq!(View::Model.url_prefix(), "");
    assert_eq!(View::Kernel.url_prefix(), "kernels/");
    assert_eq!(cfg.public_url.as_str(), format!("{PUBLIC_URL}/"));
}

/// A page scans at most ten rows per entry it may list: a caller who reads
/// few of many repositories gets short, even empty, pages, each linking
/// the next, and still sees every repository it may read.
#[tokio::test]
async fn sparse_listings_scan_a_bounded_window_per_page() {
    let hub = TestHub::start(
        vec![
            user_token(ALICE, "alice", &[]),
            user_token("oxt_test_reader", "reader", &[]),
        ],
        vec![
            rule(&["alice"], &["*"], ALL),
            rule(&["reader"], &["z/*"], &[openxet_auth::Action::Read]),
        ],
    )
    .await;
    for i in 0..25 {
        hub.create(ALICE, "a", &format!("r{i:02}"), None).await;
    }
    hub.create(ALICE, "z", "last", None).await;
    let mut next = "/api/models?limit=1".to_owned();
    let mut pages = Vec::new();
    loop {
        let page = hub
            .call(Method::GET, &next, Some("oxt_test_reader"), None)
            .await;
        assert_eq!(page.status, StatusCode::OK);
        pages.push(
            ids(&page.json())
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>(),
        );
        let Some(link) = page.header("link") else {
            break;
        };
        next = link
            .strip_prefix(&format!("<{PUBLIC_URL}"))
            .and_then(|l| l.strip_suffix(">; rel=\"next\""))
            .unwrap()
            .to_owned();
    }
    assert!(pages[0].is_empty(), "{pages:?}");
    assert_eq!(pages.concat(), ["z/last"]);
    assert!(pages.len() >= 3, "{pages:?}");
}
