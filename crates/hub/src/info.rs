//! `GET /api/{type}s/{id}[/revision/{rev}]` and `GET
//! /api/{type}s/{id}/commits/{rev}`: a repository's info at a revision and
//! its history (design §6, S3).

use axum::Json;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::header::LINK;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use openxet_git::{CommitInfo, EntryKind, SnapshotRepo};
use openxet_index::Oid;
use serde_json::{Value, json};

use crate::authz::RepoAccess;
use crate::listing::{decode_cursor, next_link, page_limit, paged};
use crate::refs::at_revision;
use crate::repos::{repo_json, repo_schema, timestamp};
use crate::routes::{Doc, Routes, on, repo, with_revision};
use crate::{HubError, HubState, param};

/// `X-Total-Count`: how many commits the history holds (S3 fixture `21`).
const X_TOTAL_COUNT: HeaderName = HeaderName::from_static("x-total-count");

/// The info and commits routes of every Hub type.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new();
    let info = typed_paths!("/{ns}/{name}");
    let at = typed_paths!("/{ns}/{name}/revision/{rev}");
    let commits = typed_paths!("/{ns}/{name}/commits/{rev}");
    for i in 0..info.len() {
        routes = routes
            .route(info[i], on(Method::GET, info_doc(), info_route).private())
            .route(at[i], on(Method::GET, info_doc(), info_route).private())
            .route(
                commits[i],
                on(Method::GET, commits_doc(), commits_route).private(),
            );
    }
    routes
}

fn info_doc() -> Doc {
    let sibling = json!({
        "type": "object",
        "required": ["rfilename"],
        "properties": {
            "rfilename": {"type": "string", "description": "The file's path."},
            "blobId": {"type": "string", "description": "With `blobs`."},
            "size": {"type": "integer", "description": "With `blobs`."},
            "lfs": {
                "type": "object",
                "description": "With `blobs`, for an LFS file.",
                "properties": {
                    "sha256": {"type": "string"},
                    "size": {"type": "integer"},
                    "pointerSize": {"type": "integer"}
                }
            }
        }
    });
    with_revision(repo("Repository info"))
        .describe(
            "The repository, with its files at the revision: `main` without one. A \
             repository whose initial commit failed answers without `sha`.",
        )
        .query(
            "blobs",
            false,
            &json!({"type": "boolean"}),
            "Each sibling with its `blobId`, `size` and, for an LFS file, `lfs`.",
        )
        .answer(
            StatusCode::OK,
            "The repository.",
            Some((
                "application/json",
                repo_schema(json!({
                    "sha": {"type": "string", "description": "The revision's commit."},
                    "lastModified": {"type": "string", "format": "date-time"},
                    "siblings": {"type": "array", "items": sibling}
                })),
            )),
        )
}

fn commits_doc() -> Doc {
    let doc = with_revision(repo("List commits"))
        .describe("The history at the revision, newest first.")
        .answer(
            StatusCode::OK,
            "A page of commits.",
            Some((
                "application/json",
                json!({
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["id", "title", "message", "authors", "date"],
                        "properties": {
                            "id": {"type": "string"},
                            "title": {"type": "string"},
                            "message": {"type": "string"},
                            "authors": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "user": {"type": "string"},
                                        "avatar": {"type": "null"}
                                    }
                                }
                            },
                            "date": {"type": "string", "format": "date-time"}
                        }
                    }
                }),
            )),
        )
        .answer_header(
            StatusCode::OK,
            "X-Total-Count",
            "On the first page, the number of commits, when it is counted within \
             `git.reachability`'s budget.",
        );
    paged(doc)
}

/// `GET …/{id}[/revision/{rev}][?blobs=true]`: the repository with `sha`,
/// `lastModified` and `siblings` at the revision, `main` by default (S3
/// fixtures `22`, `23`). With `blobs`, each sibling carries its `blobId`,
/// `size` and, for an LFS file, `lfs {sha256, size, pointerSize}`: the
/// sibling names the SHA-256 `sha256` where a tree entry names it `oid`.
async fn info_route(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    RawQuery(query): RawQuery,
) -> Result<Json<Value>, HubError> {
    let blobs = url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
        .any(|(key, value)| key == "blobs" && matches!(&*value, "true" | "True" | "1"));
    let rev = param(&params, "rev");
    let mut json = repo_json(access.view, &access.repo);
    let snap = state.engine.snapshot(access.repo.id).await?;
    // A repository whose initial commit failed has no refs yet; its info
    // still answers, without a revision.
    if rev.is_empty() && snap.refs.is_empty() {
        json["siblings"] = json!([]);
        return Ok(Json(json));
    }
    drop(snap);
    let rev = if rev.is_empty() {
        "main".to_owned()
    } else {
        rev
    };
    let (snap, resolved) = at_revision(&state, &access, &rev).await?;
    let head = last_commit(&state, &access, &snap, resolved.commit).await?;
    let entries = state
        .engine
        .read_tree(&snap, resolved.commit, "", true)
        .await?;
    let mut siblings = Vec::new();
    for entry in &entries {
        let EntryKind::File { oid, size, lfs } = &entry.kind else {
            continue;
        };
        let mut sibling = json!({ "rfilename": entry.path });
        if blobs {
            sibling["blobId"] = oid.to_string().into();
            sibling["size"] = lfs.map_or(*size, |p| p.size).into();
            if let Some(pointer) = lfs {
                sibling["lfs"] = json!({
                    "sha256": pointer.sha256_hex(),
                    "size": pointer.size,
                    "pointerSize": size,
                });
            }
        }
        siblings.push(sibling);
    }
    json["sha"] = resolved.commit.to_string().into();
    if let Some(head) = head {
        json["lastModified"] = commit_date(&head).into();
    }
    json["siblings"] = siblings.into();
    Ok(Json(json))
}

/// The commit `commit` itself, as a listing shows it.
async fn last_commit(
    state: &HubState,
    access: &RepoAccess,
    snap: &SnapshotRepo,
    commit: Oid,
) -> Result<Option<CommitInfo>, HubError> {
    Ok(state
        .engine
        .log(snap, commit, None, 1, &access.who.subject)
        .await?
        .into_iter()
        .next())
}

/// When a commit was made, in huggingface.co's format.
fn commit_date(commit: &CommitInfo) -> String {
    timestamp(u64::try_from(commit.committer.time).unwrap_or(0))
}

/// `GET …/commits/{rev}`: the history, newest first, at most `limit` (and
/// `hub.page_size`) per page with `Link: <…>; rel="next"` when more follow
/// (ruling M2-R8), and `X-Total-Count` when the history is counted within
/// `git.reachability`'s budget (S3 fixture `21`). Each commit is `{id,
/// title, message, authors: [{user, avatar}], date}`, of which
/// `list_repo_commits` reads all but `avatar` (`hf_api.py` lines
/// 4191–4195); openxet holds no avatars, so `avatar` is `null`.
async fn commits_route(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    RawQuery(query): RawQuery,
    uri: Uri,
) -> Result<Response, HubError> {
    let rev = param(&params, "rev");
    let (mut cursor, mut limit) = (None, state.cfg.page_size);
    for (key, value) in url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes()) {
        match &*key {
            "cursor" => {
                let text = decode_cursor(&value)?;
                let oid = text.parse::<Oid>().map_err(|_| HubError::BadRequest {
                    message: "the cursor is not one this server issued".to_owned(),
                    hint: "Follow the `Link: <…>; rel=\"next\"` header of the previous page as \
                           it is.",
                })?;
                cursor = Some(oid);
            }
            "limit" => limit = page_limit(&value, &state.cfg)?,
            _ => {}
        }
    }
    let (snap, resolved) = at_revision(&state, &access, &rev).await?;
    let who = &access.who.subject;
    // One commit more than the page tells whether another page follows.
    let mut commits = state
        .engine
        .log(&snap, resolved.commit, cursor, limit.saturating_add(1), who)
        .await?;
    let more = commits.len() > limit;
    commits.truncate(limit);
    // Only the first page counts: a client following `Link` already has
    // the count, and each count walks the history once.
    let total = match cursor {
        None => state.engine.count_commits(&snap, resolved.commit).await?,
        Some(_) => None,
    };
    let json: Vec<Value> = commits
        .iter()
        .map(|c| {
            json!({
                "id": c.oid.to_string(),
                "title": c.title(),
                "message": c.body(),
                "authors": [{ "user": c.author.name, "avatar": null }],
                "date": commit_date(c),
            })
        })
        .collect();
    let mut response = Json(json).into_response();
    let headers = response.headers_mut();
    if let Some(total) = total {
        headers.insert(X_TOTAL_COUNT, HeaderValue::from(total));
    }
    if more && let Some(last) = commits.last() {
        let link = next_link(
            &state.cfg,
            uri.path(),
            query.as_deref(),
            &last.oid.to_string(),
        )?;
        headers.insert(LINK, link);
    }
    Ok(response)
}
