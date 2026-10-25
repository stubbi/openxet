//! Refs on the Hub API (design §6): `GET …/refs`, `POST|DELETE
//! …/branch/{branch}`, `POST …/tag/{rev}` and `DELETE …/tag/{tag}`, and
//! the revision reads every other read shares.
//!
//! Every read runs on a request snapshot, and a revision given as a commit
//! id passes R22 from the snapshot's refs as a fetch want does; ref writes
//! go through the fenced commit transaction, at a target the refs already
//! reach (ADR 0004 item 6).

use axum::Json;
use axum::body::Body;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::Method;
use openxet_auth::Action;
use openxet_git::{Ident, Resolved, Revision, SnapshotRepo, TagInfo};
use openxet_index::{Oid, RefUpdate};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess};
use crate::routes::{Routes, on};
use crate::tree::on_flag;
use crate::{CONTROL_BODY_LIMIT, HubError, HubState, param, parse_json, read_body};

/// The ref routes of every Hub type.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new();
    let refs = typed_paths!("/{ns}/{name}/refs");
    let branch = typed_paths!("/{ns}/{name}/branch/{branch}");
    let tag = typed_paths!("/{ns}/{name}/tag/{rev}");
    for i in 0..refs.len() {
        routes = routes
            .route(refs[i], on(Method::GET, list).private())
            .route(
                branch[i],
                on(Method::POST, create_branch)
                    .on(Method::DELETE, delete_branch)
                    .private(),
            )
            .route(
                tag[i],
                on(Method::POST, create_tag)
                    .on(Method::DELETE, delete_tag)
                    .private(),
            );
    }
    routes
}

/// A revision as the client wrote it: a commit id when it is 40 hex
/// characters, else a ref name.
pub(crate) fn revision(rev: &str) -> Revision<'_> {
    match rev.parse::<Oid>() {
        Ok(oid) if rev.len() == 40 => Revision::Oid(oid),
        _ => Revision::Ref(rev),
    }
}

/// A request snapshot of `access`'s repository and `rev` resolved in it.
///
/// # Errors
///
/// [`HubError::RevisionNotFound`] for a revision the snapshot doesn't
/// have or R22 refuses, [`HubError::Busy`] over R22's limits, and the
/// snapshot's failures.
pub(crate) async fn at_revision(
    state: &HubState,
    access: &RepoAccess,
    rev: &str,
) -> Result<(SnapshotRepo, Resolved), HubError> {
    let snap = state.engine.snapshot(access.repo.id).await?;
    let resolved = state
        .engine
        .resolve_revision(&snap, revision(rev), &access.who.subject)
        .await?;
    Ok((snap, resolved))
}

/// The target of the full ref `name` in `snap`.
pub(crate) fn ref_target(snap: &SnapshotRepo, name: &str) -> Option<Oid> {
    snap.refs
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, oid)| *oid)
}

/// One ref as `list_repo_refs` reads it (`hf_api.py` line 4109).
fn ref_json(short: &str, full: &str, target: Oid) -> Value {
    json!({ "name": short, "ref": full, "targetCommit": target.to_string() })
}

/// `GET …/refs[?include_prs=1]`: `{"tags","branches","converts"}`, and
/// `"pullRequests"` with `include_prs` (S3 fixtures `14`, `15`). A tag's
/// `targetCommit` is what its ref names, the annotated tag object, as
/// huggingface.co answers.
async fn list(
    State(state): State<HubState>,
    access: RepoAccess,
    RawQuery(query): RawQuery,
) -> Result<Json<Value>, HubError> {
    let include_prs = url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes())
        .any(|(key, value)| key == "include_prs" && on_flag(&value));
    let snap = state.engine.snapshot(access.repo.id).await?;
    let (mut tags, mut branches, mut pulls) = (Vec::new(), Vec::new(), Vec::new());
    for (name, target) in &snap.refs {
        if let Some(short) = name.strip_prefix("refs/tags/") {
            tags.push(ref_json(short, name, *target));
        } else if let Some(short) = name.strip_prefix("refs/heads/") {
            branches.push(ref_json(short, name, *target));
        } else if name.starts_with("refs/pr/") {
            pulls.push(ref_json(name, name, *target));
        }
    }
    let mut body = json!({ "tags": tags, "branches": branches, "converts": [] });
    if include_prs {
        body["pullRequests"] = pulls.into();
    }
    Ok(Json(body))
}

/// `POST …/branch/{branch}`'s body: `{}`, or `{"startingPoint"}`
/// (`hf_api.py` line 6889).
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BranchBody {
    starting_point: Option<String>,
}

/// `body` as JSON of type `T`, or `T::default()` for an empty body.
async fn optional_json<T: for<'de> Deserialize<'de> + Default>(body: Body) -> Result<T, HubError> {
    let bytes = read_body(body, CONTROL_BODY_LIMIT).await?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    parse_json(&bytes)
}

/// `POST …/branch/{branch}`: creates the branch at `startingPoint`, by
/// default `main`'s tip. `200 {"success":true}` (S3 fixture `11`), or
/// `409` naming the ref if it exists (fixture `12`), which
/// `create_branch(exist_ok=True)` swallows. Needs `write`.
async fn create_branch(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    body: Body,
) -> Result<Json<Value>, HubError> {
    authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    let body: BranchBody = optional_json(body).await?;
    let name = format!("refs/heads/{}", param(&params, "branch"));
    let start = body.starting_point.as_deref().unwrap_or("main");
    let (_, resolved) = at_revision(&state, &access, start).await?;
    let update = RefUpdate {
        name: name.clone(),
        old: None,
        new: Some(resolved.commit),
    };
    write_ref(&state, &access, update, None).await?;
    Ok(Json(json!({ "success": true })))
}

/// `DELETE …/branch/{branch}`: `200 {"success":true}` (S3 fixture `31`).
/// `main` can't be deleted. Needs `write`.
async fn delete_branch(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
) -> Result<Json<Value>, HubError> {
    authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    let branch = param(&params, "branch");
    delete_ref(&state, &access, &format!("refs/heads/{branch}"), &branch).await?;
    Ok(Json(json!({ "success": true })))
}

/// `POST …/tag/{rev}`'s body: `{"tag","message"?}` (`hf_api.py` line
/// 7025).
#[derive(Deserialize)]
struct TagBody {
    tag: String,
    message: Option<String>,
}

/// `POST …/tag/{rev}`: tags the revision with an annotated tag by the
/// caller, as huggingface.co does (S3 fixtures `13`, `14`). `200 {}`.
/// Needs `write`.
async fn create_tag(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    body: Body,
) -> Result<Json<Value>, HubError> {
    authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    let body: TagBody = parse_json(&read_body(body, CONTROL_BODY_LIMIT).await?)?;
    let (_, resolved) = at_revision(&state, &access, &param(&params, "rev")).await?;
    let update = RefUpdate {
        name: format!("refs/tags/{}", body.tag),
        old: None,
        new: Some(resolved.commit),
    };
    let tag = TagInfo {
        message: body.message.unwrap_or_default(),
        tagger: Ident::author(&access.who),
    };
    write_ref(&state, &access, update, Some(tag)).await?;
    Ok(Json(json!({})))
}

/// `DELETE …/tag/{tag}`: `200 {}` (S3 fixture `30`). Needs `write`.
async fn delete_tag(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
) -> Result<Json<Value>, HubError> {
    authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    let tag = param(&params, "rev");
    delete_ref(&state, &access, &format!("refs/tags/{tag}"), &tag).await?;
    Ok(Json(json!({})))
}

/// Writes `update`, answering a ref that isn't where the update expects
/// with huggingface.co's text for one that exists (S3 fixture `12`).
async fn write_ref(
    state: &HubState,
    access: &RepoAccess,
    update: RefUpdate,
    tag: Option<TagInfo>,
) -> Result<(), HubError> {
    let name = update.name.clone();
    match state
        .engine
        .hub_ref_write(access.repo.id, &access.who, update, tag)
        .await
    {
        Err(openxet_git::GitError::RefConflict { .. }) => Err(HubError::RefConflict {
            message: format!("Reference {name} already exists"),
        }),
        other => Ok(other?),
    }
}

/// Deletes the full ref `name`, which the client called `short`.
async fn delete_ref(
    state: &HubState,
    access: &RepoAccess,
    name: &str,
    short: &str,
) -> Result<(), HubError> {
    let snap = state.engine.snapshot(access.repo.id).await?;
    let old = ref_target(&snap, name).ok_or_else(|| HubError::RevisionNotFound {
        revision: short.to_owned(),
    })?;
    drop(snap);
    let update = RefUpdate {
        name: name.to_owned(),
        old: Some(old),
        new: None,
    };
    match state
        .engine
        .hub_ref_write(access.repo.id, &access.who, update, None)
        .await
    {
        Err(openxet_git::GitError::RefConflict { .. }) => Err(HubError::RefConflict {
            message: format!("Reference {name} moved while it was being deleted; retry"),
        }),
        other => Ok(other?),
    }
}

#[cfg(test)]
mod tests {
    use openxet_git::Revision;

    use super::revision;

    #[test]
    fn revisions_are_ids_only_at_40_hex() {
        let id = "0123456789abcdef0123456789abcdef01234567";
        assert!(matches!(revision(id), Revision::Oid(_)));
        assert_eq!(revision("main"), Revision::Ref("main"));
        assert_eq!(revision(&id[..39]), Revision::Ref(&id[..39]));
        assert_eq!(revision("refs/pr/1"), Revision::Ref("refs/pr/1"));
    }
}
