//! `POST /api/{type}s/{id}/commit/{rev}`: Hub commits (ruling M2-R7).
//!
//! A Hub commit is a real Git commit built on the branch tip, published
//! through the fenced commit transaction like a push (ADR 0004 item 6).
//! An `lfsFile` becomes a canonical LFS pointer, and only for a file whose
//! SHA-256 was registered to this repository's scope by a shard upload:
//! the SHA-256 is client-asserted, so a file uploaded to another
//! repository never answers for this one (design §5.4, Review Focus 1).

use axum::Json;
use axum::body::Body;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::{Method, StatusCode};
use openxet_auth::Action;
use openxet_git::Op;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess};
use crate::ndjson::{Limits, MAX_COMMIT_OPS, parse_commit_ndjson};
use crate::routes::{Doc, Routes, on, repo, with_body, with_revision, with_write};
use crate::{HubError, HubState, param, read_body};

/// The commit route of every Hub type.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new();
    for path in typed_paths!("/{ns}/{name}/commit/{rev}") {
        routes = routes.route(path, on(Method::POST, commit_doc(), commit).private());
    }
    routes
}

/// The commit body: one `{"key", "value"}` object per line.
fn commit_body() -> Value {
    let line = |key: &str, value: Value| {
        json!({
            "type": "object",
            "required": ["key", "value"],
            "properties": {"key": {"type": "string", "enum": [key]}, "value": value}
        })
    };
    let path = || json!({"type": "object", "required": ["path"], "properties": {"path": {"type": "string"}}});
    json!({
        "description": "One `{\"key\", \"value\"}` object per line: a `header` first, then \
            the operations.",
        "oneOf": [
            line("header", json!({
                "type": "object",
                "required": ["summary"],
                "properties": {
                    "summary": {"type": "string"},
                    "description": {"type": "string"},
                    "parentCommit": {
                        "type": "string",
                        "description": "The tip the commit expects; `412` if it moved."
                    }
                }
            })),
            line("file", json!({
                "type": "object",
                "required": ["path", "content", "encoding"],
                "properties": {
                    "path": {"type": "string"},
                    "content": {"type": "string"},
                    "encoding": {"type": "string", "enum": ["base64"]}
                }
            })),
            line("lfsFile", json!({
                "type": "object",
                "required": ["path", "algo", "oid", "size"],
                "properties": {
                    "path": {"type": "string"},
                    "algo": {"type": "string", "enum": ["sha256"]},
                    "oid": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                    "size": {"type": "integer", "minimum": 0}
                }
            })),
            line("deletedFile", path()),
            line("deletedFolder", path())
        ]
    })
}

fn commit_doc() -> Doc {
    with_write(with_body(with_revision(repo("Commit"))))
        .describe(
            "A Git commit on the branch `{rev}`, built on its tip. An `lfsFile` must name a \
             file uploaded to this repository through Xet, by its SHA-256 and size. At most \
             `hub.max_commit_bytes` and 25,000 operations.",
        )
        .query(
            "create_pr",
            false,
            &json!({"type": "boolean"}),
            "Pull requests are not implemented: a true value answers `501`.",
        )
        .body(
            "application/x-ndjson",
            &commit_body(),
            "The commit, as `huggingface_hub` sends it.",
        )
        .answer(
            StatusCode::OK,
            "Committed.",
            Some((
                "application/json",
                json!({
                    "type": "object",
                    "required": ["success", "commitOid", "commitUrl"],
                    "properties": {
                        "success": {"type": "boolean", "enum": [true]},
                        "commitOid": {"type": "string"},
                        "commitUrl": {"type": "string"},
                        "hookOutput": {"type": "string"}
                    }
                }),
            )),
        )
        .error(
            StatusCode::BAD_REQUEST,
            "`invalid_write`: a bad path, or objects git refuses. An `lfsFile` not uploaded \
             to this repository is `bad_request`.",
        )
        .error(
            StatusCode::PRECONDITION_FAILED,
            "`precondition_failed`: `parentCommit` is not the branch's tip.",
        )
        .error(
            StatusCode::CONFLICT,
            "`ref_conflict`: the branch moved while the commit was written.",
        )
        .error(
            StatusCode::NOT_IMPLEMENTED,
            "`not_implemented`: `create_pr`.",
        )
        .error(
            StatusCode::SERVICE_UNAVAILABLE,
            "`busy`: the commit lost its branch to concurrent writers on every attempt. \
             `Retry-After: 1`.",
        )
}

const UPLOAD_HINT: &str = "Upload the file to this repository first: `hf upload` (or \
     `upload_file` with hf_xet installed) sends it through Xet, which registers it here. A \
     file uploaded to another repository doesn't count for this one.";

/// `POST …/commit/{rev}`: `200 {"success":true,"commitOid","commitUrl",
/// "hookOutput":""}` (S3 fixture `09-…-14`). Needs `write`. The body is at
/// most `hub.max_commit_bytes` and 25,000 operations (ruling M2-R4); a
/// `parentCommit` that isn't the branch tip answers `412`.
async fn commit(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    RawQuery(query): RawQuery,
    body: Body,
) -> Result<Json<Value>, HubError> {
    authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    let create_pr = url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
        .any(|(key, value)| key == "create_pr" && value != "0" && value != "false");
    if create_pr {
        return Err(HubError::NotImplemented {
            endpoint: "pull requests (`create_pr=True`)",
        });
    }
    let limit = usize::try_from(state.cfg.max_commit_bytes).unwrap_or(usize::MAX);
    let body = read_body(body, limit).await?;
    let limits = Limits {
        max_file_bytes: state.cfg.lfs_threshold,
        max_ops: MAX_COMMIT_OPS,
    };
    let request = parse_commit_ndjson(&body, &limits)?;
    drop(body);
    let scope = format!("repo:{}", access.repo.id);
    for op in &request.ops {
        let Op::LfsFile { path, sha256, size } = op else {
            continue;
        };
        let registered = state.index.file_by_sha256(&scope, sha256).await?;
        if registered.is_none_or(|file| file.size != *size) {
            return Err(HubError::BadRequest {
                message: format!(
                    "{path} names a file of {size} bytes that was not uploaded to this repository"
                ),
                hint: UPLOAD_HINT,
            });
        }
    }
    let branch = param(&params, "rev");
    let oid = state
        .engine
        .hub_commit(access.repo.id, &access.who, request.on(&branch))
        .await
        .map_err(|e| HubError::at(e, &branch, None))?;
    let url = format!(
        "{}/commit/{oid}",
        access
            .view
            .repo_url(&state.cfg, &access.repo.namespace, &access.repo.name)
    );
    tracing::info!(repo = %access.repo.id, commit = %oid, "hub commit");
    Ok(Json(json!({
        "success": true,
        "commitOid": oid.to_string(),
        "commitUrl": url,
        "hookOutput": "",
    })))
}
