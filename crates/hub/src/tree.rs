//! `GET /api/{type}s/{id}/tree/{rev}[/{path}]` and `POST
//! /api/{type}s/{id}/paths-info/{rev}`: a revision's entries (design §6,
//! S3).
//!
//! Paths are read inside the revision's commit tree only: a `.` or `..`
//! component, like a missing path, answers `EntryNotFound` (Review Focus
//! 4). An LFS file's `xetHash` is the Xet file this repository's scope
//! registered with its SHA-256; a pointer whose SHA-256 the scope doesn't
//! know lists without one.

use std::collections::{HashMap, HashSet};

use axum::Json;
use axum::body::Body;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::header::LINK;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use openxet_git::{
    CommitInfo, Entry, EntryKind, GitError, Pointer, Resolved, SnapshotRepo, TreePage,
};
use openxet_index::XetFile;
use openxet_protocol::hash_to_hex;
use serde::Serialize;
use serde_json::{Value, json};

use crate::authz::RepoAccess;
use crate::listing::{decode_cursor, next_link, page_limit, paged};
use crate::refs::at_revision;
use crate::repos::timestamp;
use crate::routes::{Doc, Routes, on, repo, with_body, with_revision};
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
            .route(tree_root[i], on(Method::GET, tree_doc(), tree).private())
            .route(tree_path[i], on(Method::GET, tree_doc(), tree).private())
            .route(
                paths_info[i],
                on(Method::POST, paths_info_doc(), paths_info_route).private(),
            );
    }
    routes
}

/// An entry of a tree listing or `paths-info`.
fn entry_schema() -> Value {
    let scan = json!({"type": "object", "properties": {"status": {"type": "string"}}});
    json!({
        "type": "object",
        "required": ["type", "oid", "size", "path"],
        "properties": {
            "type": {"type": "string", "enum": ["file", "directory"]},
            "oid": {"type": "string", "description": "The Git object id."},
            "size": {"type": "integer", "description": "An LFS file's own size."},
            "path": {"type": "string"},
            "lfs": {
                "type": "object",
                "description": "For an LFS file.",
                "properties": {
                    "oid": {"type": "string", "description": "The file's SHA-256."},
                    "size": {"type": "integer"},
                    "pointerSize": {"type": "integer"}
                }
            },
            "xetHash": {
                "type": "string",
                "description": "For an LFS file this repository registered: its Xet hash."
            },
            "lastCommit": {
                "type": "object",
                "description": "With `expand`.",
                "properties": {
                    "id": {"type": "string"},
                    "title": {"type": "string"},
                    "date": {"type": "string", "format": "date-time"}
                }
            },
            "securityFileStatus": {
                "type": "object",
                "description": "With `expand`; OpenXet runs no scans, so each is `unscanned`.",
                "properties": {
                    "status": {"type": "string"},
                    "protectAiScan": scan,
                    "avScan": scan,
                    "pickleImportScan": scan,
                    "virusTotalScan": scan
                }
            }
        }
    })
}

fn entry_not_found(doc: Doc) -> Doc {
    doc.error(
        StatusCode::NOT_FOUND,
        "`entry_not_found`: the path names nothing at the revision. `X-Error-Code: \
         EntryNotFound`, `X-Repo-Commit`.",
    )
}

fn tree_doc() -> Doc {
    let doc = entry_not_found(with_revision(repo("List a folder")))
        .describe(
            "The folder's entries at the revision, or with `recursive` everything under it, \
             ordered by path. Paths are read inside the commit's tree only.",
        )
        .query(
            "recursive",
            false,
            &json!({"type": "boolean"}),
            "Everything under the folder.",
        )
        .query(
            "expand",
            false,
            &json!({"type": "boolean"}),
            "Add `lastCommit` and `securityFileStatus`.",
        )
        .answer(
            StatusCode::OK,
            "A page of entries.",
            Some((
                "application/json",
                json!({"type": "array", "items": entry_schema()}),
            )),
        );
    paged(doc)
}

fn paths_info_doc() -> Doc {
    with_body(with_revision(repo("Entries at paths")))
        .describe(
            "The entries at the paths, in request order, each path at most once; a path that \
             names nothing is left out.",
        )
        .body(
            "application/x-www-form-urlencoded",
            &json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Up to 1000; repeat the field once per path."
                    },
                    "expand": {"type": "boolean"}
                }
            }),
            "The paths, form-encoded, at most 1 MiB.",
        )
        .answer(
            StatusCode::OK,
            "The entries.",
            Some((
                "application/json",
                json!({"type": "array", "items": entry_schema()}),
            )),
        )
}

/// Whether a boolean query parameter is on: `true` or `1`, as
/// `huggingface_hub` and browsers write it.
pub(crate) fn on_flag(value: &str) -> bool {
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
    // Entries are ordered by path bytes; a cursor is the last path listed.
    let page = TreePage {
        after: cursor.as_deref(),
        limit,
    };
    let listing = state
        .engine
        .read_tree_page(&snap, resolved.commit, &path, recursive, page)
        .await
        .map_err(|e| HubError::at(e, &rev, Some(resolved.commit)))?;
    let json = entries_json(&state, &access, &snap, resolved, &listing.entries, expand).await?;
    let mut response = Json(json).into_response();
    if listing.more
        && let Some(last) = listing.entries.last()
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
) -> Result<Response, HubError> {
    let rev = param(&params, "rev");
    let body = read_body(body, PATHS_INFO_BODY_LIMIT).await?;
    let mut paths: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut fields = 0usize;
    let mut expand = false;
    for (key, value) in url::form_urlencoded::parse(&body) {
        match &*key {
            "paths" => {
                // Every `paths` field counts, repeats included, so the
                // limit bounds the parsing work, not only the lookups.
                fields += 1;
                if fields > MAX_PATHS {
                    return Err(HubError::BadRequest {
                        message: format!("paths-info takes at most {MAX_PATHS} paths at a time"),
                        hint: "Ask about the paths in batches, as get_paths_info does.",
                    });
                }
                if seen.insert(value.clone().into_owned()) {
                    paths.push(value.into_owned());
                }
            }
            "expand" => expand = on_flag(&value),
            _ => {}
        }
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
    let json = entries_json(&state, &access, &snap, resolved, &found, expand).await?;
    Ok(Json(json).into_response())
}

/// `path` without empty components, or `None` for the root or a path with
/// a `.` or `..` component, which name no entry.
fn normalize(path: &str) -> Option<String> {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    (!comps.is_empty() && comps.iter().all(|c| *c != "." && *c != "..")).then(|| comps.join("/"))
}

/// `entries` as the Hub lists them, with `lastCommit` and
/// `securityFileStatus` when `expand`.
async fn entries_json<'a>(
    state: &HubState,
    access: &RepoAccess,
    snap: &SnapshotRepo,
    resolved: Resolved,
    entries: &'a [Entry],
    expand: bool,
) -> Result<Vec<EntryJson<'a>>, HubError> {
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
    // One batch of lookups for the page's Xet files, not one per entry.
    let pointers: Vec<[u8; 32]> = entries
        .iter()
        .filter_map(|e| match &e.kind {
            EntryKind::File { lfs: Some(p), .. } => Some(p.sha256),
            _ => None,
        })
        .collect();
    let xet = state.index.xet_files_by_sha256(&scope, &pointers).await?;
    Ok(entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let mut json = entry_json(entry, &xet);
            if expand {
                json.last_commit = last.get(i).and_then(Option::as_ref).map(last_commit_json);
                if matches!(entry.kind, EntryKind::File { .. }) {
                    json.security_file_status = Some(security_status());
                }
            }
            json
        })
        .collect())
}

/// One entry as the Hub lists it (`hf_api.py` lines 692–743), serialized
/// straight from this struct: a page holds up to 1,000, and building each
/// as a `serde_json::Value` map cost a page about as much as reading its
/// blobs did.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EntryJson<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    oid: String,
    size: u64,
    path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    lfs: Option<LfsJson>,
    #[serde(skip_serializing_if = "Option::is_none")]
    xet_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_commit: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    security_file_status: Option<Value>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LfsJson {
    oid: String,
    size: u64,
    pointer_size: u64,
}

/// One entry: a file's `oid` is its blob, the pointer's for an LFS file,
/// whose `size` is the file's and whose `lfs` names the file's SHA-256 as
/// `oid` (S3 fixture `16`). `xet` holds the Xet files the scope
/// registered, by SHA-256.
fn entry_json<'a>(entry: &'a Entry, xet: &HashMap<[u8; 32], XetFile>) -> EntryJson<'a> {
    let (kind, oid, size, lfs) = match &entry.kind {
        EntryKind::Dir { oid } => ("directory", oid, 0, None),
        EntryKind::File { oid, size, lfs } => ("file", oid, lfs.map_or(*size, |p| p.size), {
            lfs.map(|p| (p, *size))
        }),
    };
    EntryJson {
        kind,
        oid: oid.to_string(),
        size,
        path: &entry.path,
        lfs: lfs.map(|(pointer, pointer_size)| LfsJson {
            oid: pointer.sha256_hex(),
            size: pointer.size,
            pointer_size,
        }),
        xet_hash: lfs
            .and_then(|(pointer, _)| xet.get(&pointer.sha256).filter(|f| f.size == pointer.size))
            .map(|file| hash_to_hex(&file.hash)),
        last_commit: None,
        security_file_status: None,
    }
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
