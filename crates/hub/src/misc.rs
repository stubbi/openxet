//! `POST /api/validate-yaml`, and the Hub endpoints openxet doesn't
//! implement, which answer `501` naming themselves (design §6).

use std::future::{Ready, ready};

use axum::Json;
use axum::body::Body;
use axum::http::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess, Who};
use crate::routes::{Area, Doc, Handlers, Routes, api, on, repo, with_body};
use crate::{HubError, HubState, json_body};

/// The largest `validate-yaml` body: room for a model card as large as a
/// regular file may be (`hub.lfs_threshold` is at most 10 MiB), JSON-escaped.
const VALIDATE_YAML_BODY_LIMIT: usize = 16 << 20;

/// The miscellaneous routes.
pub(crate) fn routes() -> Routes {
    use Method as M;
    let mut routes = Routes::new()
        .route(
            "/api/validate-yaml",
            on(M::POST, validate_yaml_doc(), validate_yaml).private(),
        )
        // The `hf` CLI fetches it at most once a day to detect AI agents and
        // ignores any failure (`utils/_detect_agent.py` lines 197–204).
        .route(
            "/api/agent-harnesses",
            unserved(&[M::GET], "Agent harnesses", "agent-harnesses"),
        )
        .route(
            "/api/collections",
            unserved(&[M::GET, M::POST], "Collections", "collections"),
        )
        .route(
            "/api/collections/{*rest}",
            unserved(
                &[M::GET, M::POST, M::PATCH, M::DELETE],
                "A collection",
                "collections",
            ),
        )
        .route(
            "/api/users/{user}/likes",
            unserved(&[M::GET], "A user's likes", "likes"),
        )
        .route(
            "/api/models-tags-by-type",
            unserved(&[M::GET], "Model card tags", "model card tags"),
        )
        .route(
            "/api/datasets-tags-by-type",
            unserved(&[M::GET], "Dataset card tags", "dataset card tags"),
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
                unserved_repo(&[M::GET, M::POST], "Discussions", "discussions"),
            )
            .route(
                discussion[i],
                unserved_repo(
                    &[M::GET, M::POST, M::PATCH, M::DELETE],
                    "A discussion",
                    "discussions",
                ),
            )
            .route(
                like[i],
                unserved_repo(&[M::POST, M::DELETE], "Like", "likes"),
            )
            .route(likers[i], unserved_repo(&[M::GET], "Likers", "likes"));
    }
    routes
}

const NOT_IMPLEMENTED: &str = "`not_implemented`: OpenXet doesn't implement this Hub \
     endpoint; the message names it.";

/// `methods` of a Hub endpoint OpenXet doesn't implement, doing `summary`:
/// `501` naming `endpoint`, never a confusing `404` (design §6).
fn unserved(methods: &[Method], summary: &'static str, endpoint: &'static str) -> Handlers {
    let doc = api(Area::Hub, summary).error(StatusCode::NOT_IMPLEMENTED, NOT_IMPLEMENTED);
    each(methods, &doc, &move |_: Who| refuse(endpoint))
}

/// [`unserved`] on a repository, answered after the repository is
/// resolved.
fn unserved_repo(methods: &[Method], summary: &'static str, endpoint: &'static str) -> Handlers {
    let doc = repo(summary).error(StatusCode::NOT_IMPLEMENTED, NOT_IMPLEMENTED);
    each(methods, &doc, &move |_: RepoAccess| refuse(endpoint)).private()
}

/// `handler`, described by `doc`, for each of `methods`.
///
/// # Panics
///
/// If `methods` is empty.
fn each<H, T>(methods: &[Method], doc: &Doc, handler: &H) -> Handlers
where
    H: axum::handler::Handler<T, HubState> + Clone,
    T: 'static,
{
    let (first, rest) = methods
        .split_first()
        .unwrap_or_else(|| unreachable!("no methods"));
    rest.iter().fold(
        on(first.clone(), doc.clone(), handler.clone()),
        |handlers, method| handlers.on(method.clone(), doc.clone(), handler.clone()),
    )
}

fn validate_yaml_doc() -> Doc {
    with_body(api(Area::Hub, "Validate a model card"))
        .describe(
            "Every commit adding a `README.md` posts it here first. OpenXet checks the \
             request's shape, not the card's metadata.",
        )
        .body(
            "application/json",
            &json!({
                "type": "object",
                "required": ["content"],
                "properties": {"content": {"type": "string"}}
            }),
            "The card, at most 16 MiB.",
        )
        .answer(
            StatusCode::OK,
            "No errors and no warnings.",
            Some((
                "application/json",
                json!({
                    "type": "object",
                    "required": ["errors", "warnings"],
                    "properties": {
                        "errors": {"type": "array"},
                        "warnings": {"type": "array"}
                    }
                }),
            )),
        )
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
