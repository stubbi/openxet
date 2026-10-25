//! `POST /api/validate-yaml`, and the Hub endpoints openxet doesn't
//! implement, which answer `501` naming themselves (design §6).

use std::future::{Ready, ready};

use axum::Json;
use axum::body::Body;
use axum::http::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess, Who};
use crate::routes::{Routes, on};
use crate::{HubError, json_body};

/// The largest `validate-yaml` body: room for a model card as large as a
/// regular file may be (`hub.lfs_threshold` is at most 10 MiB), JSON-escaped.
const VALIDATE_YAML_BODY_LIMIT: usize = 16 << 20;

/// The miscellaneous routes.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new()
        .route(
            "/api/validate-yaml",
            on(Method::POST, validate_yaml).private(),
        )
        // The `hf` CLI fetches it at most once a day to detect AI agents and
        // ignores any failure (`utils/_detect_agent.py` lines 197–204).
        .route(
            "/api/agent-harnesses",
            on(Method::GET, |_: Who| refuse("agent-harnesses")),
        )
        .route(
            "/api/collections",
            on(Method::GET, |_: Who| refuse("collections"))
                .on(Method::POST, |_: Who| refuse("collections")),
        )
        .route(
            "/api/collections/{*rest}",
            on(Method::GET, |_: Who| refuse("collections"))
                .on(Method::POST, |_: Who| refuse("collections"))
                .on(Method::PATCH, |_: Who| refuse("collections"))
                .on(Method::DELETE, |_: Who| refuse("collections")),
        )
        .route(
            "/api/users/{user}/likes",
            on(Method::GET, |_: Who| refuse("likes")),
        )
        .route(
            "/api/models-tags-by-type",
            on(Method::GET, |_: Who| refuse("model card tags")),
        )
        .route(
            "/api/datasets-tags-by-type",
            on(Method::GET, |_: Who| refuse("dataset card tags")),
        );
    // On a repository, the `501` comes after the repository is resolved, so
    // these routes answer a repository the caller may not read exactly as
    // every other repository route does (ruling M2-R6).
    let discussions = typed_paths!("/{ns}/{name}/discussions");
    let discussion = typed_paths!("/{ns}/{name}/discussions/{*rest}");
    let like = typed_paths!("/{ns}/{name}/like");
    let likers = typed_paths!("/{ns}/{name}/likers");
    for i in 0..discussions.len() {
        routes = routes
            .route(
                discussions[i],
                on(Method::GET, |_: RepoAccess| refuse("discussions"))
                    .on(Method::POST, |_: RepoAccess| refuse("discussions"))
                    .private(),
            )
            .route(
                discussion[i],
                on(Method::GET, |_: RepoAccess| refuse("discussions"))
                    .on(Method::POST, |_: RepoAccess| refuse("discussions"))
                    .on(Method::PATCH, |_: RepoAccess| refuse("discussions"))
                    .on(Method::DELETE, |_: RepoAccess| refuse("discussions"))
                    .private(),
            )
            .route(
                like[i],
                on(Method::POST, |_: RepoAccess| refuse("likes"))
                    .on(Method::DELETE, |_: RepoAccess| refuse("likes"))
                    .private(),
            )
            .route(
                likers[i],
                on(Method::GET, |_: RepoAccess| refuse("likes")).private(),
            );
    }
    routes
}

/// The handler answer of an endpoint openxet doesn't implement.
fn refuse(endpoint: &'static str) -> Ready<HubError> {
    ready(HubError::NotImplemented { endpoint })
}

/// `POST /api/validate-yaml`'s body (S3 fixture `09-…-01`).
#[derive(Deserialize)]
struct ValidateYaml {
    #[serde(rename = "content")]
    _content: String,
}

/// `POST /api/validate-yaml`: `200 {"errors":[],"warnings":[]}`. Every
/// commit adding a `README.md` posts it here first and fails on any
/// non-2xx (`hf_api.py` lines 5124, 11469–11483, S3 answer 3). openxet
/// checks the request's shape, not the card's metadata.
async fn validate_yaml(Who(who): Who, body: Body) -> Result<Json<Value>, HubError> {
    if authz::is_anonymous(&who) {
        return Err(HubError::MissingCredential);
    }
    let _: ValidateYaml = json_body(body, VALIDATE_YAML_BODY_LIMIT).await?;
    Ok(Json(json!({ "errors": [], "warnings": [] })))
}
