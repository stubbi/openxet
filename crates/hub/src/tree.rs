//! `GET /api/{type}s/{id}/tree/{rev}[/{path}]` and `POST
//! /api/{type}s/{id}/paths-info/{rev}`: a revision's entries (design §6,
//! S3).
//!
//! Paths are read inside the revision's commit tree only: a `.` or `..`
//! component, like a missing path, answers `EntryNotFound` (Review Focus
//! 4). An LFS file's `xetHash` is the Xet file this repository's scope
//! registered with its SHA-256; a pointer whose SHA-256 the scope doesn't
//! know lists without one.

use std::collections::HashMap;

use axum::Json;
use axum::body::Body;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::header::LINK;
use axum::http::{Method, Uri};
use axum::response::{IntoResponse, Response};
use openxet_git::{CommitInfo, Entry, EntryKind, GitError, Pointer, Resolved, SnapshotRepo};
use openxet_protocol::hash_to_hex;
use serde_json::{Value, json};

use crate::authz::RepoAccess;
use crate::listing::{decode_cursor, next_link, page_limit};
use crate::refs::at_revision;
use crate::repos::timestamp;
use crate::routes::{Routes, on};
use crate::{HubError, HubState, param, read_body};

/// The largest `paths-info` body, form-encoded paths.
const PATHS_INFO_BODY_LIMIT: usize = 1 << 20;

/// The most paths one `paths-info` request may ask about. `get_paths_info`
/// sends every path its caller passes in one request (`hf_api.py` line
/// 4270), so this bounds the work of one request, not a client batch.
const MAX_PATHS: usize = 1000;

/// The tree and paths-info routes of every Hub type.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new();
    let tree_root = typed_paths!("/{ns}/{name}/tree/{rev}");
    let tree_path = typed_paths!("/{ns}/{name}/tree/{rev}/{*path}");
    let paths_info = typed_paths!("/{ns}/{name}/paths-info/{rev}");
    for i in 0..tree_root.len() {
        routes = routes
            .route(tree_root[i], on(Method::GET, tree).private())
            .route(tree_path[i], on(Method::GET, tree).private())
            .route(paths_info[i], on(Method::POST, paths_info_route).private());
    }
    routes
}

/// Whether a boolean query parameter is on: `true` or `1`, as
/// `huggingface_hub` and browsers write it.
fn on_flag(value: &str) -> bool {
    matches!(value, "true" | "True" | "1")
}

/// `GET …/tree/{rev}[/{path}]?recursive=&expand=`: the folder's entries,
/// or with `recursive` everything under it, ordered by path, at most
/// `limit` (and `hub.page_size`) per page with `Link: <…>; rel="next"`
/// when more follow (ruling M2-R8). `expand` adds `lastCommit` and
/// `securityFileStatus` (S3 fixtures `16`–`19`).
async fn tree(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    RawQuery(query): RawQuery,
    uri: Uri,
) -> Result<Response, HubError> {
    let (rev, path) = (param(&params, "rev"), param(&params, "path"));
    let (mut recursive, mut expand) = (false, false);
    let (mut cursor, mut limit) = (None, state.cfg.page_size);
    for (key, value) in url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes()) {
        match &*key {
            "recursive" => recursive = on_flag(&value),
            "expand" => expand = on_flag(&value),
            "cursor" => cursor = Some(decode_cursor(&value)?),
            "limit" => limit = page_limit(&value, &state.cfg)?,
            _ => {}
        }
    }
    let (snap, resolved) = at_revision(&state, &access, &rev).await?;
    let entries = state
        .engine
        .read_tree(&snap, resolved.commit, &path, recursive)
        .await
        .map_err(|e| HubError::at(e, &rev, Some(resolved.commit)))?;
    // Entries are ordered by path bytes; a cursor is the last path listed.
    let start = cursor.map_or(0, |c| {
        entries.partition_point(|e| e.path.as_bytes() <= c.as_bytes())
    });
    let end = entries.len().min(start.saturating_add(limit));
    let page = &entries[start..end];
    let json = entries_json(&state, &access, &snap, resolved, page, expand).await?;
    let mut response = Json(json).into_response();
    if end < entries.len()
        && let Some(last) = page.last()
    {
        let link = next_link(&state.cfg, uri.path(), query.as_deref(), &last.path)?;
        response.headers_mut().insert(LINK, link);
    }
    Ok(response)
}

/// `POST …/paths-info/{rev}`, form-encoded `paths=…&paths=…&expand=`
/// (`hf_api.py` lines 4270–4273): the entries at those paths, in request
/// order, each path at most once; a path that names nothing is left out
/// (S3 fixture `20`).
async fn paths_info_route(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    body: Body,
) -> Result<Json<Value>, HubError> {
    let rev = param(&params, "rev");
    let body = read_body(body, PATHS_INFO_BODY_LIMIT).await?;
    let mut paths: Vec<String> = Vec::new();
    let mut expand = false;
    for (key, value) in url::form_urlencoded::parse(&body) {
        match &*key {
            "paths" if !paths.iter().any(|p| *p == value) => paths.push(value.into_owned()),
            "expand" => expand = on_flag(&value),
            _ => {}
        }
    }
    if paths.len() > MAX_PATHS {
        return Err(HubError::BadRequest {
            message: format!("paths-info takes at most {MAX_PATHS} paths at a time"),
            hint: "Ask about the paths in batches, as get_paths_info does.",
        });
    }
    let (snap, resolved) = at_revision(&state, &access, &rev).await?;
    // Each path is looked up in its folder's listing, read once per folder.
    let mut folders: HashMap<String, Vec<Entry>> = HashMap::new();
    let mut found = Vec::new();
    for path in &paths {
        let Some(normal) = normalize(path) else {
            continue;
        };
        let parent = normal.rsplit_once('/').map_or("", |(parent, _)| parent);
        if !folders.contains_key(parent) {
            let listing = match state
                .engine
                .read_tree(&snap, resolved.commit, parent, false)
                .await
            {
                Ok(listing) => listing,
                Err(GitError::EntryNotFound { .. }) => Vec::new(),
                Err(e) => return Err(e.into()),
            };
            folders.insert(parent.to_owned(), listing);
        }
        if let Some(entry) = folders[parent].iter().find(|e| e.path == normal) {
            found.push(entry.clone());
        }
    }
    Ok(Json(
        entries_json(&state, &access, &snap, resolved, &found, expand).await?,
    ))
}

/// `path` without empty components, or `None` for the root or a path with
/// a `.` or `..` component, which name no entry.
fn normalize(path: &str) -> Option<String> {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    (!comps.is_empty() && comps.iter().all(|c| *c != "." && *c != "..")).then(|| comps.join("/"))
}

/// `entries` as the Hub lists them, with `lastCommit` and
/// `securityFileStatus` when `expand`.
async fn entries_json(
    state: &HubState,
    access: &RepoAccess,
    snap: &SnapshotRepo,
    resolved: Resolved,
    entries: &[Entry],
    expand: bool,
) -> Result<Value, HubError> {
    let scope = format!("repo:{}", access.repo.id);
    let last = if expand {
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        state
            .engine
            .last_commit_for_paths(snap, resolved.commit, &paths)
            .await?
    } else {
        Vec::new()
    };
    let mut out = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        let mut json = entry_json(state, &scope, entry).await?;
        if expand {
            if let Some(commit) = last.get(i).and_then(Option::as_ref) {
                json["lastCommit"] = last_commit_json(commit);
            }
            if matches!(entry.kind, EntryKind::File { .. }) {
                json["securityFileStatus"] = security_status();
            }
        }
        out.push(json);
    }
    Ok(Value::Array(out))
}

/// One entry (`hf_api.py` lines 692–743): a file's `oid` is its blob, the
/// pointer's for an LFS file, whose `size` is the file's and whose `lfs`
/// names the file's SHA-256 as `oid` (S3 fixture `16`).
async fn entry_json(state: &HubState, scope: &str, entry: &Entry) -> Result<Value, HubError> {
    Ok(match &entry.kind {
        EntryKind::Dir { oid } => json!({
            "type": "directory",
            "oid": oid.to_string(),
            "size": 0,
            "path": entry.path,
        }),
        EntryKind::File { oid, size, lfs } => {
            let mut json = json!({
                "type": "file",
                "oid": oid.to_string(),
                "size": lfs.map_or(*size, |p| p.size),
                "path": entry.path,
            });
            if let Some(pointer) = lfs {
                json["lfs"] = json!({
                    "oid": pointer.sha256_hex(),
                    "size": pointer.size,
                    "pointerSize": size,
                });
                if let Some(hash) = xet_hash(state, scope, pointer).await? {
                    json["xetHash"] = hash.into();
                }
            }
            json
        }
    })
}

/// The Xet file hash `scope` registered for `pointer`'s SHA-256 and size.
pub(crate) async fn xet_hash(
    state: &HubState,
    scope: &str,
    pointer: &Pointer,
) -> Result<Option<String>, HubError> {
    Ok(state
        .index
        .file_by_sha256(scope, &pointer.sha256)
        .await?
        .filter(|file| file.size == pointer.size)
        .map(|file| hash_to_hex(&file.hash)))
}

/// A commit as `lastCommit`: `{id, title, date}` (S3 fixture `17`).
fn last_commit_json(commit: &CommitInfo) -> Value {
    json!({
        "id": commit.oid.to_string(),
        "title": commit.title(),
        "date": timestamp(u64::try_from(commit.committer.time).unwrap_or(0)),
    })
}

/// `securityFileStatus` with every scan `unscanned`: openxet runs none, and
/// `huggingface_hub` requires `status`, `avScan` and `pickleImportScan`
/// when the object is present (`hf_api.py` lines 708–713).
fn security_status() -> Value {
    let unscanned = json!({ "status": "unscanned" });
    json!({
        "status": "unscanned",
        "protectAiScan": unscanned,
        "avScan": unscanned,
        "pickleImportScan": unscanned,
        "virusTotalScan": unscanned,
    })
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalized_paths_stay_inside_the_tree() {
        assert_eq!(normalize("/a//b/").as_deref(), Some("a/b"));
        assert_eq!(normalize("a"), Some("a".to_owned()));
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("a/../b"), None);
        assert_eq!(normalize("./a"), None);
    }
}
