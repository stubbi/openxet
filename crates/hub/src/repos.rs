//! Repositories on the Hub API: create, delete, info and the typed
//! listings (design §6, rulings M2-R2 and M2-R8, ADR 0010).

use std::fmt::Write as _;
use std::time::{Duration, UNIX_EPOCH};

use axum::Json;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::header::{CONTENT_TYPE, LINK};
use axum::http::{HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openxet_auth::{Action, UserPrincipal};
use openxet_index::{AuditEntry, IndexError, Repo, RepoFilter, RepoId};
use openxet_storage::{RepoIdHex, RepoMeta};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::authz::{self, RepoAccess, Who};
use crate::routes::{Routes, on};
use crate::{HubConfig, HubError, HubState, View, json_body, labels};

/// Namespaces that are route prefixes, where no repository may be created
/// (ADR 0012 item 4).
pub const RESERVED_NAMESPACES: [&str; 11] = [
    "models",
    "datasets",
    "spaces",
    "kernels",
    "buckets",
    "api",
    "v1",
    "v2",
    "openapi.json",
    "healthz",
    "readyz",
];

/// The longest namespace or name (ruling M2-R2).
const MAX_SEGMENT: usize = 96;

const SEGMENT_HINT: &str = "A namespace and a name each match ^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$: \
     1 to 96 letters, digits, `.`, `_` and `-`, starting with a letter or digit (ruling M2-R2).";

const DOTS_HINT: &str = "A namespace or name may not contain `..` (ruling M2-R2); choose one \
     without it.";

const GIT_SUFFIX_HINT: &str = "A name may not end in `.git`, which Git URLs append (ruling \
     M2-R2); drop the suffix.";

const RESERVED_HINT: &str = "The namespaces models, datasets, spaces, kernels, buckets, api, v1, \
     v2, openapi.json, healthz and readyz are route prefixes (ADR 0012); choose another \
     namespace.";

/// Checks `{namespace}/{name}` against ruling M2-R2.
///
/// # Errors
///
/// [`HubError::InvalidName`] naming the part and the rule it breaks.
pub fn check_name(namespace: &str, name: &str) -> Result<(), HubError> {
    check_segment("namespace", namespace)?;
    check_segment("name", name)?;
    if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".git") {
        return Err(invalid(
            format!("the name {name:?} ends in `.git`"),
            GIT_SUFFIX_HINT,
        ));
    }
    Ok(())
}

fn check_segment(what: &str, segment: &str) -> Result<(), HubError> {
    let wrong = if segment.is_empty() {
        "is empty"
    } else if segment.len() > MAX_SEGMENT {
        "is longer than 96 characters"
    } else if !segment.as_bytes()[0].is_ascii_alphanumeric() {
        "does not start with a letter or digit"
    } else if !segment
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        "holds a character other than letters, digits, `.`, `_` and `-`"
    } else if segment.contains("..") {
        return Err(invalid(
            format!("the {what} {segment:?} contains `..`"),
            DOTS_HINT,
        ));
    } else {
        return Ok(());
    };
    Err(invalid(
        format!("the {what} {segment:?} {wrong}"),
        SEGMENT_HINT,
    ))
}

fn invalid(message: String, hint: &'static str) -> HubError {
    HubError::InvalidName { message, hint }
}

/// `{namespace}/{name}` as stored.
pub(crate) fn full_name(repo: &Repo) -> String {
    format!("{}/{}", repo.namespace, repo.name)
}

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
    if RESERVED_NAMESPACES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(namespace))
    {
        return Err(invalid(
            format!("the namespace {namespace:?} is reserved"),
            RESERVED_HINT,
        ));
    }
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
    let meta = meta_of(&repo)?;
    state
        .bucket
        .put_replace(meta.key(), meta.to_bytes())
        .await?;
    match state.index.create_repo(&repo).await {
        Ok(()) => {}
        Err(IndexError::NameTaken { .. }) => {
            return Err(match state.index.repo_by_name(namespace, name).await? {
                Some(existing) => taken(state, who, &existing, namespace, name, view).await,
                None => HubError::NameTaken {
                    message: format!("Repository {namespace}/{name} already exists"),
                    url: view.repo_url(&state.cfg, namespace, name),
                },
            });
        }
        Err(e) => return Err(e.into()),
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

/// The repository's bucket metadata record.
pub(crate) fn meta_of(repo: &Repo) -> Result<RepoMeta, HubError> {
    Ok(RepoMeta {
        id: RepoIdHex::parse(&repo.id.to_string())?,
        namespace: repo.namespace.clone(),
        name: repo.name.clone(),
        private: repo.private,
        labels: repo.labels.clone(),
        created_at: repo.created_at,
    })
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

/// The Hub's repository routes: create, delete, and per type the listing
/// and repository info.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new()
        .route("/api/repos/create", on(Method::POST, create).private())
        .route("/api/repos/delete", on(Method::DELETE, delete).private());
    let listings = typed_paths!("");
    let infos = typed_paths!("/{ns}/{name}");
    for ((view, listing), info) in View::TYPED.into_iter().zip(listings).zip(infos) {
        routes = routes
            .route(
                listing,
                on(
                    Method::GET,
                    move |state: State<HubState>, who: Who, query: RawQuery| {
                        list(state, who, query, view)
                    },
                )
                .private(),
            )
            .route(info, on(Method::GET, info_route).private());
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
    let body: CreateBody = json_body(body).await?;
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
/// namespace: it frees the name for anyone who may create there. Only the
/// index rows go; the repository's data leaves through GC
/// (non-negotiable 3).
async fn delete(
    State(state): State<HubState>,
    Who(who): Who,
    body: Body,
) -> Result<Response, HubError> {
    if authz::is_anonymous(&who) {
        return Err(HubError::MissingCredential);
    }
    let body: DeleteBody = json_body(body).await?;
    let view = typed_view(body.kind.as_deref())?;
    let namespace = body.organization.unwrap_or_else(|| who.name.clone());
    let access = RepoAccess::resolve(&state, who, &namespace, &body.name, view).await?;
    let repo = &access.repo;
    authz::require_as(&state, &access.who, Action::Write, repo, "delete").await?;
    authz::require_as(&state, &access.who, Action::Create, repo, "delete").await?;
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

/// The listing parameters openxet answers: `author` (a namespace), `limit`
/// (at most `hub.page_size` per page) and `cursor` (ruling M2-R8). `full`,
/// `config`, `cardData` and `expand` ask for optional fields and are
/// answered with what openxet has; any other parameter filters or sorts,
/// which openxet doesn't, so it answers `501` rather than an unfiltered
/// listing.
const OPTIONAL_FIELDS: [&str; 4] = ["full", "config", "cardData", "expand"];

/// `GET /api/{type}s`: the repositories with the view's label that the
/// caller may read, `limit` at a time, ordered by `{namespace}/{name}`
/// ignoring case, with `Link: <…>; rel="next"` while more follow, which
/// `huggingface_hub` follows (`utils/_pagination.py` line 51).
async fn list(
    State(state): State<HubState>,
    Who(who): Who,
    RawQuery(query): RawQuery,
    view: View,
) -> Result<Response, HubError> {
    let mut author = None;
    let mut limit = state.cfg.page_size;
    let mut cursor = None;
    for (key, value) in url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes()) {
        match &*key {
            "author" => author = Some(value.into_owned()),
            "limit" => {
                limit = value
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| HubError::BadRequest {
                        message: format!("limit {value:?} is not a positive whole number"),
                        hint: "Pass `limit` as a whole number from 1; pages hold at most \
                               hub.page_size entries.",
                    })?
                    .min(state.cfg.page_size);
            }
            "cursor" => cursor = Some(decode_cursor(&value)?),
            key if OPTIONAL_FIELDS.contains(&key) => {}
            _ => {
                return Err(HubError::NotImplemented {
                    endpoint: "listing repositories filtered or sorted by anything but `author`",
                });
            }
        }
    }
    let filter = RepoFilter {
        label: view.label().map(str::to_owned),
        namespace: author,
    };
    let (items, next) = readable_page(&state, &who, filter, cursor, limit).await?;
    let json: Vec<Value> = items.iter().map(|repo| repo_json(view, repo)).collect();
    let mut response = Json(json).into_response();
    if let Some(next) = next {
        let link = next_link(&state.cfg, view, query.as_deref(), &next);
        if let Ok(value) = HeaderValue::from_str(&link) {
            response.headers_mut().insert(LINK, value);
        }
    }
    Ok(response)
}

/// Up to `limit` repositories matching `filter` that `who` may read,
/// starting after the index cursor `cursor`, and the index cursor after
/// the last one scanned while more may follow. Each index page asks for no
/// more than are still wanted, so every entry it returns is either listed
/// or skipped, and its cursor resumes exactly after the page.
async fn readable_page(
    state: &HubState,
    who: &UserPrincipal,
    filter: RepoFilter,
    mut cursor: Option<String>,
    limit: usize,
) -> Result<(Vec<Repo>, Option<String>), HubError> {
    let mut items = Vec::new();
    loop {
        let page = state
            .index
            .list_repos(filter.clone(), cursor.as_deref(), limit - items.len())
            .await?;
        for repo in page.items {
            if authz::allowed(state, who, Action::Read, &repo.namespace, Some(&repo.name)).await? {
                items.push(repo);
            }
        }
        cursor = page.next;
        if items.len() == limit || cursor.is_none() {
            return Ok((items, cursor));
        }
    }
}

/// The listing's next page: this request's URL with `cursor` replaced.
fn next_link(cfg: &HubConfig, view: View, query: Option<&str>, next: &str) -> String {
    let mut url = cfg.public_url.clone();
    url.set_path(&format!("/api/{}s", view.noun()));
    let kept: Vec<(String, String)> = url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .filter(|(key, _)| key != "cursor")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(kept)
        .append_pair("cursor", &URL_SAFE_NO_PAD.encode(next));
    format!("<{url}>; rel=\"next\"")
}

/// The index cursor a `cursor` parameter carries, base64url-encoded
/// (ruling M2-R8).
fn decode_cursor(text: &str) -> Result<String, HubError> {
    URL_SAFE_NO_PAD
        .decode(text)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| HubError::BadRequest {
            message: "the cursor is not one this server issued".to_owned(),
            hint: "Follow the `Link: <…>; rel=\"next\"` header of the previous page as it is.",
        })
}
