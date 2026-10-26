//! Repositories on the Hub API: create, delete and info (design §6,
//! ruling M2-R2, ADR 0010).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::{Duration, UNIX_EPOCH};

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::{Action, UserPrincipal};
use openxet_git::{HubCommit, Ident, Op};
use openxet_index::{AuditEntry, IndexError, Repo, RepoId};
use openxet_storage::{RepoIdHex, RepoMeta};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess, Who};
use crate::names::{check_name, check_not_reserved, full_name};
use crate::preupload::DEFAULT_GITATTRIBUTES;
use crate::routes::{Area, Doc, Routes, api, on, with_body, with_write};
use crate::{CONTROL_BODY_LIMIT, HubError, HubState, View, json_body, labels};

/// Creates the repository `{namespace}/{name}` for `who`, carrying
/// `label`.
///
/// Creation is authorized on the namespace first, so a caller who may not
/// create there gets the same `403` whether or not the name exists (ADR
/// 0010 item 5). Then the name is checked (ruling M2-R2), and the
/// repository's metadata record is written to the bucket before the index
/// row (design §5.3), then the audit row and the initial commit (ruling
/// M2-R5).
///
/// # Errors
///
/// [`HubError::MissingCredential`] or [`HubError::CreateForbidden`] for a
/// caller who may not create in `namespace`, [`HubError::InvalidName`] or
/// [`HubError::InvalidLabel`] for a bad name or label,
/// [`HubError::NameTaken`] for a name taken ignoring case, and the
/// storage, index, Git engine and authorizer failures.
pub async fn create_repo(
    state: &HubState,
    who: &UserPrincipal,
    namespace: &str,
    name: &str,
    private: bool,
    label: &str,
) -> Result<Repo, HubError> {
    create_with(state, who, namespace, name, private, label, true).await
}

/// [`create_repo`] without the initial commit: the repository a first
/// `git push` creates (ruling M2-R11), whose refs are the push's own. An
/// initial commit on `main` would make the push's update of `main` a
/// conflict.
///
/// # Errors
///
/// Those of [`create_repo`].
pub(crate) async fn create_repo_for_push(
    state: &HubState,
    who: &UserPrincipal,
    namespace: &str,
    name: &str,
    label: &str,
) -> Result<Repo, HubError> {
    create_with(state, who, namespace, name, false, label, false).await
}

async fn create_with(
    state: &HubState,
    who: &UserPrincipal,
    namespace: &str,
    name: &str,
    private: bool,
    label: &str,
    with_initial_commit: bool,
) -> Result<Repo, HubError> {
    let view = View::from_label(label).unwrap_or(View::Any);
    if !authz::allowed(state, who, Action::Create, namespace, None).await? {
        return Err(if authz::is_anonymous(who) {
            HubError::MissingCredential
        } else {
            HubError::CreateForbidden {
                noun: view.noun(),
                namespace: namespace.to_owned(),
            }
        });
    }
    check_name(namespace, name)?;
    check_not_reserved(namespace)?;
    labels::check_label(label)?;
    // Checked before the bucket write, so an ordinary conflict leaves no
    // metadata record behind; the index's unique key still decides a race.
    if let Some(existing) = state.index.repo_by_name(namespace, name).await? {
        return Err(taken(state, who, &existing, namespace, name, view).await);
    }
    let id = RepoId::generate();
    let repo = Repo {
        id,
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        private,
        labels: vec![label.to_owned()],
        created_at: state.clock.now(),
        full_repack_gen: 0,
    };
    let meta = meta_of(&repo, None)?;
    state
        .bucket
        .put_replace(meta.key(), meta.to_bytes())
        .await?;
    if let Err(err) = state.index.create_repo(&repo).await {
        // The record reached the bucket; if the row didn't, tombstone the
        // record so a reindex never creates the repository. Only once a
        // read shows the row absent: a commit whose acknowledgement was
        // lost has a live row, which a tombstone would hide from reindex.
        match state.index.repo_by_id(id).await {
            Ok(None) => tombstone(state, &repo, "failed create").await,
            Ok(Some(_)) => {}
            Err(e) => tracing::error!(
                repo = %id,
                error = &e as &dyn std::error::Error,
                "could not check a failed create's index row; its metadata record stays live"
            ),
        }
        return Err(match err {
            IndexError::NameTaken { .. } => {
                match state.index.repo_by_name(namespace, name).await? {
                    Some(existing) => taken(state, who, &existing, namespace, name, view).await,
                    None => HubError::NameTaken {
                        message: format!("Repository {namespace}/{name} already exists"),
                        url: view.repo_url(&state.cfg, namespace, name),
                    },
                }
            }
            other => other.into(),
        });
    }
    // The repository exists from here on, so its creation is audited before
    // the initial commit, whatever that commit's outcome.
    state
        .index
        .audit(&AuditEntry {
            actor: who.subject.clone(),
            action: "repo.create".to_owned(),
            repo: Some(id),
            detail: json!({
                "namespace": namespace,
                "name": name,
                "private": private,
                "labels": [label],
            }),
        })
        .await?;
    if with_initial_commit {
        initial_commit(state, id).await?;
    }
    tracing::info!(repo = %id, "repository created");
    Ok(repo)
}

/// Writes a new repository's `initial commit` on `main`, holding
/// huggingface.co's default `.gitattributes`, by `openxet
/// <noreply@openxet.com>` (ruling M2-R5), so `commits`, `refs` and `git
/// clone` behave as on huggingface.co. Should it fail, the repository
/// exists without refs, and the first commit to it creates `main` (its
/// preupload reads the same default `.gitattributes`), so a retried upload
/// with `exist_ok=True` still lands.
async fn initial_commit(state: &HubState, id: RepoId) -> Result<(), HubError> {
    let ident = Ident::openxet();
    let openxet = UserPrincipal {
        subject: ident.name.clone(),
        name: ident.name,
        email: Some(ident.email),
        claims: BTreeMap::new(),
    };
    let commit = HubCommit {
        branch: "main",
        parent: None,
        summary: "initial commit",
        description: "",
        ops: vec![Op::File {
            path: ".gitattributes".to_owned(),
            content: Bytes::from_static(DEFAULT_GITATTRIBUTES),
        }],
    };
    state.engine.hub_commit(id, &openxet, commit).await?;
    Ok(())
}

/// The repository's bucket metadata record, tombstoned at `deleted_at`.
pub(crate) fn meta_of(repo: &Repo, deleted_at: Option<u64>) -> Result<RepoMeta, HubError> {
    Ok(RepoMeta {
        id: RepoIdHex::parse(&repo.id.to_string())?,
        namespace: repo.namespace.clone(),
        name: repo.name.clone(),
        private: repo.private,
        labels: repo.labels.clone(),
        created_at: repo.created_at,
        deleted_at,
    })
}

/// Tombstones `repo`'s record, best effort: the caller's outcome is
/// already decided, and a record left live is what `reindex` meets as a
/// duplicate name or an orphan, so a failure here is logged for the
/// operator rather than answered. `why` names the case in the log.
async fn tombstone(state: &HubState, repo: &Repo, why: &'static str) {
    let written = match meta_of(repo, Some(state.clock.now())) {
        Ok(meta) => state
            .bucket
            .put_replace(meta.key(), meta.to_bytes())
            .await
            .map_err(HubError::from),
        Err(e) => Err(e),
    };
    if let Err(err) = written {
        tracing::error!(
            repo = %repo.id,
            error = &err as &dyn std::error::Error,
            why,
            "could not tombstone a repository's metadata record"
        );
    }
}

/// The `409` for a create of `{namespace}/{name}` that `existing` holds.
/// Its labels are named only to a caller who may read it (ADR 0010 item
/// 5); `url` is the requested view's URL of the requested name.
async fn taken(
    state: &HubState,
    who: &UserPrincipal,
    existing: &Repo,
    namespace: &str,
    name: &str,
    view: View,
) -> HubError {
    let url = view.repo_url(&state.cfg, namespace, name);
    let readable = authz::allowed(
        state,
        who,
        Action::Read,
        &existing.namespace,
        Some(&existing.name),
    )
    .await;
    let message = match readable {
        Ok(true) => {
            let mut message = format!(
                "Repository {namespace}/{name} already exists, with the labels [{}]",
                existing.labels.join(", ")
            );
            if let Some(label) = view.label().filter(|_| !view.admits(existing)) {
                let _ = write!(
                    message,
                    "; to use it as a {label}, add the label: openxet label add {}/{} {label}",
                    existing.namespace, existing.name
                );
            }
            message
        }
        Ok(false) => format!("Repository {namespace}/{name} already exists"),
        Err(e) => return e,
    };
    HubError::NameTaken { message, url }
}

/// The Hub's repository routes: create and delete. The info route is
/// [`crate::info`]'s.
pub(crate) fn routes() -> Routes {
    Routes::new()
        .route(
            "/api/repos/create",
            on(Method::POST, create_doc(), create).private(),
        )
        .route(
            "/api/repos/delete",
            on(Method::DELETE, delete_doc(), delete).private(),
        )
}

/// The body both routes take: which repository.
fn naming(more: Value) -> Value {
    let mut properties = json!({
        "name": {"type": "string", "description": "The repository's name."},
        "organization": {
            "type": "string",
            "description": "The namespace; the caller's own name if left out."
        },
        "type": {
            "type": "string",
            "enum": ["model", "dataset", "space", "kernel"],
            "description": "The label the repository is seen through; `model` if left out."
        }
    });
    if let (Some(all), Value::Object(more)) = (properties.as_object_mut(), more) {
        all.extend(more);
    }
    json!({"type": "object", "required": ["name"], "properties": properties})
}

fn create_doc() -> Doc {
    with_body(api(Area::Hub, "Create a repository"))
        .describe(
            "Creates `{organization}/{name}` with an initial commit on `main`, labelled \
             `type`. Creation is authorized on the namespace first, so a caller who may not \
             create there gets the same `403` whether or not the name exists. Names are \
             unique ignoring case and across labels.",
        )
        .body(
            "application/json",
            &naming(json!({"visibility": {"type": "string", "enum": ["public", "private"]}})),
            "The repository to create.",
        )
        .answer(
            StatusCode::OK,
            "Created.",
            Some((
                "application/json",
                json!({
                    "type": "object",
                    "required": ["url", "name", "id"],
                    "properties": {
                        "url": {"type": "string", "description": "The repository's URL."},
                        "name": {"type": "string", "description": "`{namespace}/{name}`."},
                        "id": {"type": "string"},
                        "filesCopyPending": {"type": "boolean"}
                    }
                }),
            )),
        )
        .error(
            StatusCode::BAD_REQUEST,
            "`invalid_name`: the namespace or name breaks a naming rule; the hint names it. \
             `invalid_label`: the label breaks a rule.",
        )
        .error(
            StatusCode::FORBIDDEN,
            "`forbidden`: the caller may not create in the namespace.",
        )
        .error(
            StatusCode::CONFLICT,
            "`name_taken`: the name is taken, ignoring case. The body's `url` is the \
             repository's URL, which `create_repo(exist_ok=True)` reads; its labels are \
             named only to a caller who may read it.",
        )
}

fn delete_doc() -> Doc {
    with_write(with_body(api(Area::Hub, "Delete a repository")))
        .describe(
            "Needs `write` on the repository and `create` in its namespace. The repository \
             is gone at once; its data leaves through garbage collection.",
        )
        .body(
            "application/json",
            &naming(json!({})),
            "The repository to delete.",
        )
        .answer(
            StatusCode::OK,
            "Deleted: `OK`.",
            Some(("text/plain", json!({"type": "string"}))),
        )
        .error(
            StatusCode::NOT_FOUND,
            "`repo_not_found`: no such repository, or one the caller may not read; \
             `delete_repo(missing_ok=True)` treats it as done.",
        )
}

/// [`repo_json`]'s shape: the fields the info and listing routes share.
pub(crate) fn repo_schema(more: Value) -> Value {
    let mut properties = json!({
        "_id": {"type": "string"},
        "id": {"type": "string", "description": "`{namespace}/{name}`."},
        "author": {"type": "string", "description": "The namespace."},
        "private": {"type": "boolean"},
        "tags": {"type": "array", "items": {"type": "string"}},
        "downloads": {"type": "integer"},
        "likes": {"type": "integer"},
        "gated": {"type": "boolean"},
        "disabled": {"type": "boolean"},
        "createdAt": {"type": "string", "format": "date-time"},
        "labels": {
            "type": "array",
            "items": {"type": "string"},
            "description": "OpenXet's labels (ADR 0010)."
        },
        "modelId": {"type": "string", "description": "Models only."},
        "config": {"type": "object", "description": "Models only."},
        "spaces": {"type": "array", "items": {"type": "string"}, "description": "Models only."}
    });
    if let (Some(all), Value::Object(more)) = (properties.as_object_mut(), more) {
        all.extend(more);
    }
    json!({
        "type": "object",
        "required": ["_id", "id", "author", "private", "createdAt", "labels"],
        "properties": properties
    })
}

/// `POST /api/repos/create`'s body (`hf_api.py` line 4638).
#[derive(Deserialize)]
struct CreateBody {
    name: String,
    organization: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    visibility: Option<String>,
}

/// `POST /api/repos/create`: `200 {"url","name","id","filesCopyPending"}`
/// (S3 fixture `04`), of which `create_repo` reads `url`.
async fn create(
    State(state): State<HubState>,
    Who(who): Who,
    body: Body,
) -> Result<Json<Value>, HubError> {
    if authz::is_anonymous(&who) {
        return Err(HubError::MissingCredential);
    }
    let body: CreateBody = json_body(body, CONTROL_BODY_LIMIT).await?;
    let view = typed_view(body.kind.as_deref())?;
    let private = match body.visibility.as_deref() {
        None | Some("public") => false,
        Some("private") => true,
        Some(other) => {
            return Err(HubError::BadRequest {
                message: format!("unknown visibility {other:?}"),
                hint: "Send `\"visibility\": \"private\"`, `\"public\"`, or leave it out.",
            });
        }
    };
    let namespace = body.organization.unwrap_or_else(|| who.name.clone());
    let label = view.label().unwrap_or_default();
    let repo = create_repo(&state, &who, &namespace, &body.name, private, label).await?;
    Ok(Json(json!({
        "url": view.repo_url(&state.cfg, &repo.namespace, &repo.name),
        "name": full_name(&repo),
        "id": repo.id.to_string(),
        "filesCopyPending": false,
    })))
}

/// `DELETE /api/repos/delete`'s body (`hf_api.py` line 4768).
#[derive(Deserialize)]
struct DeleteBody {
    name: String,
    organization: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// `DELETE /api/repos/delete`: `200 text/plain OK` (S3 fixtures `91`,
/// `92`), or the unknown-repository answer, which `delete_repo(missing_ok=
/// True)` swallows (fixture `93`).
///
/// Deleting needs `write` on the repository and `create` in its
/// namespace: it frees the name for anyone who may create there. The
/// metadata record is tombstoned in the bucket before the index rows go,
/// so a reindex never resurrects the repository; its data leaves through
/// GC (non-negotiable 3).
async fn delete(
    State(state): State<HubState>,
    Who(who): Who,
    body: Body,
) -> Result<Response, HubError> {
    if authz::is_anonymous(&who) {
        return Err(HubError::MissingCredential);
    }
    let body: DeleteBody = json_body(body, CONTROL_BODY_LIMIT).await?;
    let view = typed_view(body.kind.as_deref())?;
    let namespace = body.organization.unwrap_or_else(|| who.name.clone());
    let access = RepoAccess::resolve(&state, who, &namespace, &body.name, view).await?;
    let repo = &access.repo;
    authz::require_as(&state, &access.who, Action::Write, repo, "delete").await?;
    authz::require_as(&state, &access.who, Action::Create, repo, "delete").await?;
    let meta = meta_of(repo, Some(state.clock.now()))?;
    state
        .bucket
        .put_replace(meta.key(), meta.to_bytes())
        .await?;
    // If the index write fails now, the bucket is ahead: the record says
    // deleted while the row remains. The delete answers 503, and a retry
    // writes the tombstone again and removes the row, so both converge.
    if !state.index.delete_repo(repo.id).await? {
        return Err(HubError::UnknownRepo);
    }
    // A label change that read the row before it went may have written its
    // live record over the tombstone, and won its swap while the row still
    // existed. Writing the tombstone again closes that gap: a label writer
    // landing after this point finds no row, loses its swap and tombstones
    // the record itself (`labels::restore`).
    tombstone(&state, repo, "deleted repository").await;
    state
        .index
        .audit(&AuditEntry {
            actor: access.who.subject.clone(),
            action: "repo.delete".to_owned(),
            repo: Some(repo.id),
            detail: json!({ "namespace": repo.namespace, "name": repo.name }),
        })
        .await?;
    tracing::info!(repo = %repo.id, "repository deleted");
    Ok((
        [(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        )],
        "OK",
    )
        .into_response())
}

/// The view a body's `type` names, or `400`.
fn typed_view(kind: Option<&str>) -> Result<View, HubError> {
    View::from_type(kind).ok_or_else(|| HubError::BadRequest {
        message: format!("unknown repository type {:?}", kind.unwrap_or_default()),
        hint: "Send `type` as `model`, `dataset`, `space` or `kernel`, or leave it out for a \
               model.",
    })
}

/// A repository as the Hub's info and listing routes answer it, for
/// `view`: the fields huggingface.co sends that openxet knows, plus
/// openxet's `labels`; `tags` is empty and `usedStorage` left out, as
/// openxet derives neither. Datasets, spaces and
/// kernels leave out the model-only `modelId`, `config` and `spaces` (S3,
/// fixtures `22`, `42`); `huggingface_hub` requires only `id` (`hf_api.py`
/// line 894).
pub(crate) fn repo_json(view: View, repo: &Repo) -> Value {
    let id = full_name(repo);
    let mut json = json!({
        "_id": repo.id.to_string(),
        "id": id,
        "author": repo.namespace,
        "private": repo.private,
        // huggingface.co's `tags` come from model cards, which openxet
        // doesn't parse; `labels` are openxet's own (ADR 0010).
        "tags": [],
        "downloads": 0,
        "likes": 0,
        "gated": false,
        "disabled": false,
        "createdAt": timestamp(repo.created_at),
        "labels": repo.labels,
    });
    if view == View::Model
        && let Value::Object(fields) = &mut json
    {
        fields.insert("modelId".to_owned(), id.into());
        fields.insert("config".to_owned(), json!({}));
        fields.insert("spaces".to_owned(), json!([]));
    }
    json
}

/// Unix seconds as huggingface.co writes times, `%Y-%m-%dT%H:%M:%S.%fZ`
/// with milliseconds, the one form `huggingface_hub`'s `parse_datetime`
/// reads.
pub(crate) fn timestamp(secs: u64) -> String {
    humantime::format_rfc3339_millis(UNIX_EPOCH + Duration::from_secs(secs)).to_string()
}
