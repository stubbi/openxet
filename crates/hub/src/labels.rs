//! The labels API (ruling M2-R12, ADR 0010 item 3): openxet's own routes
//! under `/api/openxet/v1/`, which see a repository whatever its labels.
//!
//! Every change writes the repository's metadata record to the bucket,
//! then the index row, then an `audit_log` row, before it is answered, so
//! `reindex` restores labels and every change is attributed.

use axum::Json;
use axum::extract::{RawPathParams, State};
use axum::http::{Method, StatusCode};
use openxet_auth::Action;
use openxet_index::{AuditEntry, IndexError, Repo};
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess};
use crate::names::full_name;
use crate::repos::meta_of;
use crate::routes::{Area, Doc, Routes, api, on, with_write};
use crate::{HubError, HubState, param};

/// The longest label, in bytes (ADR 0010 item 3).
pub const MAX_LABEL_BYTES: usize = 64;

/// The most labels a repository carries (ADR 0010 item 3).
pub const MAX_LABELS: usize = 32;

/// The schema of the labels API's answers, which `openxet label --json`
/// prints too.
pub const SCHEMA: &str = "openxet.labels.v1";

const SLUG_HINT: &str = "A label is a lowercase slug of `a`-`z`, `0`-`9` and `-`, 1 to 64 bytes, \
     such as `model`, `dataset` or `eval-logs` (ADR 0010).";

const TOO_MANY_HINT: &str = "A repository carries at most 32 labels (ADR 0010); remove one first \
     with `openxet label remove`.";

/// Checks `label` against ADR 0010 item 3.
///
/// # Errors
///
/// [`HubError::InvalidLabel`] naming the rule it breaks.
pub fn check_label(label: &str) -> Result<(), HubError> {
    let wrong = if label.is_empty() {
        "is empty"
    } else if label.len() > MAX_LABEL_BYTES {
        "is longer than 64 bytes"
    } else if !label
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        "holds a character other than `a`-`z`, `0`-`9` and `-`"
    } else {
        return Ok(());
    };
    Err(HubError::InvalidLabel {
        message: format!("the label {label:?} {wrong}"),
        hint: SLUG_HINT,
    })
}

/// The labels routes.
pub(crate) fn routes() -> Routes {
    Routes::new()
        .route(
            "/api/openxet/v1/repos/{ns}/{name}/labels",
            on(Method::GET, list_doc(), list).private(),
        )
        .route(
            "/api/openxet/v1/repos/{ns}/{name}/labels/{label}",
            on(Method::PUT, add_doc(), add)
                .on(Method::DELETE, remove_doc(), remove)
                .private(),
        )
}

/// A labels route: it sees a repository whatever its labels.
fn labels_doc(summary: &'static str) -> Doc {
    api(Area::Openxet, summary)
        .answer(
            StatusCode::OK,
            "The repository's labels.",
            Some((
                "application/json",
                json!({
                    "type": "object",
                    "required": ["schema", "repo", "labels"],
                    "properties": {
                        "schema": {"type": "string", "enum": [SCHEMA]},
                        "repo": {"type": "string", "description": "`{namespace}/{name}`."},
                        "labels": {"type": "array", "items": {"type": "string"}}
                    }
                }),
            )),
        )
        .error(
            StatusCode::NOT_FOUND,
            "`repo_not_found`: no such repository, or one the caller may not read; the two \
             answer alike.",
        )
}

/// A change of labels: needs `write`, and is recorded before it is
/// answered.
fn change_doc(summary: &'static str, describe: &'static str) -> Doc {
    with_write(labels_doc(summary))
        .describe(describe)
        .error(
            StatusCode::BAD_REQUEST,
            "`invalid_label`: the label is not a lowercase slug of `a`-`z`, `0`-`9` and \
             `-`, 1 to 64 bytes, or the repository carries 32 already.",
        )
        .error(
            StatusCode::CONFLICT,
            "`labels_changed`: the labels changed concurrently; nothing was changed. Retry.",
        )
}

fn list_doc() -> Doc {
    labels_doc("List a repository's labels")
}

fn add_doc() -> Doc {
    change_doc(
        "Add a label",
        "Adding a label the repository carries changes nothing.",
    )
}

fn remove_doc() -> Doc {
    change_doc(
        "Remove a label",
        "Removing a label the repository doesn't carry changes nothing.",
    )
}

/// `GET …/labels`: `{"schema","repo","labels"}`. Needs `read`.
async fn list(access: RepoAccess) -> Json<Value> {
    Json(answer(&access.repo))
}

/// `PUT …/labels/{label}`: adds the label; adding one the repository
/// carries changes nothing. Needs `write`.
async fn add(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
) -> Result<Json<Value>, HubError> {
    change(&state, access, &param(&params, "label"), Change::Add).await
}

/// `DELETE …/labels/{label}`: removes the label; removing one the
/// repository doesn't carry changes nothing. Needs `write`.
async fn remove(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
) -> Result<Json<Value>, HubError> {
    change(&state, access, &param(&params, "label"), Change::Remove).await
}

#[derive(Clone, Copy)]
enum Change {
    Add,
    Remove,
}

async fn change(
    state: &HubState,
    access: RepoAccess,
    label: &str,
    change: Change,
) -> Result<Json<Value>, HubError> {
    authz::require_as(
        state,
        &access.who,
        Action::Write,
        &access.repo,
        "change the labels of",
    )
    .await?;
    check_label(label)?;
    // The labels this request read are the compare-and-swap's expected
    // list (ruling M2-R12). Checked against the row before the bucket is
    // written, so a request that is already stale writes nothing.
    let expected = access.repo.labels;
    let Some(mut repo) = state.index.repo_by_id(access.repo.id).await? else {
        return Err(HubError::UnknownRepo);
    };
    if repo.labels != expected {
        return Err(labels_changed(&repo));
    }
    let present = repo.labels.iter().any(|l| l == label);
    let action = match change {
        Change::Add if present => return Ok(Json(answer(&repo))),
        Change::Remove if !present => return Ok(Json(answer(&repo))),
        Change::Add => {
            if repo.labels.len() >= MAX_LABELS {
                return Err(HubError::InvalidLabel {
                    message: format!("{} already carries {MAX_LABELS} labels", full_name(&repo)),
                    hint: TOO_MANY_HINT,
                });
            }
            repo.labels.push(label.to_owned());
            "label.add"
        }
        Change::Remove => {
            repo.labels.retain(|l| l != label);
            "label.remove"
        }
    };
    let meta = meta_of(&repo, None)?;
    state
        .bucket
        .put_replace(meta.key(), meta.to_bytes())
        .await?;
    match state
        .index
        .set_labels(repo.id, &expected, &repo.labels)
        .await
    {
        Ok(()) => {}
        Err(IndexError::LabelsChanged { .. }) => {
            restore(state, &repo).await?;
            return Err(labels_changed(&repo));
        }
        Err(IndexError::UnknownRepo { .. }) => {
            restore(state, &repo).await?;
            return Err(HubError::UnknownRepo);
        }
        // The bucket is ahead of the index: the change answers 503, and a
        // retry reads the row again and rewrites both, so they converge.
        Err(e) => return Err(e.into()),
    }
    state
        .index
        .audit(&AuditEntry {
            actor: access.who.subject.clone(),
            action: action.to_owned(),
            repo: Some(repo.id),
            detail: json!({ "label": label }),
        })
        .await?;
    Ok(Json(answer(&repo)))
}

fn labels_changed(repo: &Repo) -> HubError {
    HubError::LabelsChanged {
        repo: full_name(repo),
    }
}

/// How often [`restore`] rewrites the record while the row keeps moving.
const RESTORE_ATTEMPTS: usize = 3;

/// Puts back the record a lost compare-and-swap overwrote: the row as the
/// index now has it, or a tombstone if the repository was deleted
/// meanwhile, so the bucket never keeps a change the index refused. The
/// row is read again after each write, and the record rewritten while it
/// differs, up to [`RESTORE_ATTEMPTS`] times; a writer that keeps changing
/// it rewrites the record itself.
async fn restore(state: &HubState, attempted: &Repo) -> Result<(), HubError> {
    let mut row = state.index.repo_by_id(attempted.id).await?;
    for _ in 0..RESTORE_ATTEMPTS {
        let meta = match &row {
            Some(current) => meta_of(current, None)?,
            None => meta_of(attempted, Some(state.clock.now()))?,
        };
        state
            .bucket
            .put_replace(meta.key(), meta.to_bytes())
            .await?;
        let now = state.index.repo_by_id(attempted.id).await?;
        let settled = match (&row, &now) {
            (Some(written), Some(current)) => written.labels == current.labels,
            (None, None) => true,
            _ => false,
        };
        if settled {
            break;
        }
        row = now;
    }
    Ok(())
}

fn answer(repo: &Repo) -> Value {
    json!({
        "schema": SCHEMA,
        "repo": full_name(repo),
        "labels": repo.labels,
    })
}
