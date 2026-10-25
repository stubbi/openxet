//! Repositories on the Hub API: create, delete and info (design §6,
//! ruling M2-R2, ADR 0010).

use std::fmt::Write as _;
use std::time::{Duration, UNIX_EPOCH};

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use openxet_auth::{Action, UserPrincipal};
use openxet_index::{AuditEntry, IndexError, Repo, RepoId};
use openxet_storage::{RepoIdHex, RepoMeta};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess, Who};
use crate::names::{check_name, check_not_reserved, full_name};
use crate::routes::{Routes, on};
use crate::{CONTROL_BODY_LIMIT, HubError, HubState, View, json_body, labels};

/// Creates the repository `{namespace}/{name}` for `who`, carrying
/// `label`.
///
/// Creation is authorized on the namespace first, so a caller who may not
/// create there gets the same `403` whether or not the name exists (ADR
/// 0010 item 5). Then the name is checked (ruling M2-R2), and the
/// repository's metadata record is written to the bucket before the index
/// row and the audit row (design §5.3).
///
/// # Errors
///
/// [`HubError::MissingCredential`] or [`HubError::CreateForbidden`] for a
/// caller who may not create in `namespace`, [`HubError::InvalidName`] or
/// [`HubError::InvalidLabel`] for a bad name or label,
/// [`HubError::NameTaken`] for a name taken ignoring case, and the
/// storage, index and authorizer failures.
pub async fn create_repo(
    state: &HubState,
    who: &UserPrincipal,
    namespace: &str,
    name: &str,
    private: bool,
    label: &str,
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
        // The record reached the bucket but the repository won't exist:
        // tombstone it, so a reindex from the bucket never creates it.
        tombstone(state, &repo).await;
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
    tracing::info!(repo = %id, "repository created");
    Ok(repo)
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

/// Tombstones the record of a create that won't take effect, best effort:
/// the create already fails, and a record left live is what `reindex`
/// meets as a duplicate name or an orphan, so a failure here is logged for
/// the operator rather than answered.
async fn tombstone(state: &HubState, repo: &Repo) {
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
            "could not tombstone the metadata record of a failed create"
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

/// The Hub's repository routes: create, delete, and per type the
/// repository info.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new()
        .route("/api/repos/create", on(Method::POST, create).private())
        .route("/api/repos/delete", on(Method::DELETE, delete).private());
    for info in typed_paths!("/{ns}/{name}") {
        routes = routes.route(info, on(Method::GET, info_route).private());
    }
    routes
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

/// `GET /api/{type}s/{ns}/{name}`: the repository's info (S3 fixture
/// `22`), of which `huggingface_hub` requires `id` (`hf_api.py` line 894).
async fn info_route(access: RepoAccess) -> Json<Value> {
    Json(repo_json(access.view, &access.repo))
}

/// A repository as the Hub's info and listing routes answer it, for
/// `view`: the fields huggingface.co sends that openxet knows, plus
/// openxet's `labels`. Datasets, spaces and kernels leave out the
/// model-only `modelId`, `config` and `spaces` (S3, fixtures `22`, `42`).
pub(crate) fn repo_json(view: View, repo: &Repo) -> Value {
    let id = full_name(repo);
    let mut json = json!({
        "_id": repo.id.to_string(),
        "id": id,
        "author": repo.namespace,
        "private": repo.private,
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
