//! The typed repository listings, `GET /api/{type}s` (ADR 0010 item 4,
//! ruling M2-R8).

use axum::Json;
use axum::extract::{RawQuery, State};
use axum::http::header::LINK;
use axum::http::{HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openxet_auth::{Action, UserPrincipal};
use openxet_index::{Repo, RepoFilter};
use serde_json::Value;

use crate::authz::{self, Who};
use crate::repos::repo_json;
use crate::routes::{Routes, on};
use crate::{HubConfig, HubError, HubState, View};

/// How many index rows one page may scan, per entry it may list: a caller
/// who may read few of many repositories gets short pages, each with a
/// `Link` to the next, rather than one request that scans them all.
const SCAN_FACTOR: usize = 10;

/// The listing route of each Hub type.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new();
    for (view, path) in View::TYPED.into_iter().zip(typed_paths!("")) {
        routes = routes.route(
            path,
            on(
                Method::GET,
                move |state: State<HubState>, who: Who, query: RawQuery| {
                    list(state, who, query, view)
                },
            )
            .private(),
        );
    }
    routes
}

/// The listing parameters openxet answers: `author` (a namespace), `limit`
/// (at most `hub.page_size` per page) and `cursor` (ruling M2-R8). `full`,
/// `config`, `cardData` and `expand` ask for optional fields and are
/// answered with what openxet has; any other parameter filters or sorts,
/// which openxet doesn't, so it answers `501` rather than an unfiltered
/// listing.
const OPTIONAL_FIELDS: [&str; 4] = ["full", "config", "cardData", "expand"];

/// `GET /api/{type}s`: the repositories with the view's label that the
/// caller may read, at most `limit` at a time, ordered by
/// `{namespace}/{name}` ignoring case, with `Link: <…>; rel="next"`
/// whenever the index has more, which `huggingface_hub` follows
/// (`utils/_pagination.py` line 51). A page may be short, even empty, and
/// still link the next.
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
            "limit" => limit = page_limit(&value, &state.cfg)?,
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
        let path = format!("/api/{}s", view.noun());
        let link = next_link(&state.cfg, &path, query.as_deref(), &next)?;
        response.headers_mut().insert(LINK, link);
    }
    Ok(response)
}

/// A `limit` parameter: a positive whole number, at most `hub.page_size`.
pub(crate) fn page_limit(value: &str, cfg: &HubConfig) -> Result<usize, HubError> {
    Ok(value
        .parse::<usize>()
        .ok()
        .filter(|&n| n > 0)
        .ok_or_else(|| HubError::BadRequest {
            message: format!("limit {value:?} is not a positive whole number"),
            hint: "Pass `limit` as a whole number from 1; pages hold at most hub.page_size \
                   entries.",
        })?
        .min(cfg.page_size))
}

/// Up to `limit` repositories matching `filter` that `who` may read,
/// starting after the index cursor `cursor`, scanning at most
/// [`SCAN_FACTOR`] × `limit` rows, and the index cursor after the last row
/// scanned while the index has more. Each index page asks for no more than
/// are still wanted, so every row it returns is either listed or skipped,
/// and its cursor resumes exactly after the page.
async fn readable_page(
    state: &HubState,
    who: &UserPrincipal,
    filter: RepoFilter,
    mut cursor: Option<String>,
    limit: usize,
) -> Result<(Vec<Repo>, Option<String>), HubError> {
    let mut items = Vec::new();
    let mut scanned = 0;
    loop {
        let page = state
            .index
            .list_repos(filter.clone(), cursor.as_deref(), limit - items.len())
            .await?;
        scanned += page.items.len();
        for repo in page.items {
            if authz::allowed(state, who, Action::Read, &repo.namespace, Some(&repo.name)).await? {
                items.push(repo);
            }
        }
        cursor = page.next;
        if items.len() == limit || cursor.is_none() || scanned >= SCAN_FACTOR * limit {
            return Ok((items, cursor));
        }
    }
}

/// The `Link` header of a listing's next page (ruling M2-R8): the
/// request's URL at `path` (as received, percent-encoded) with `cursor`
/// set to `next`, base64url-encoded, and the request's other parameters
/// kept.
///
/// # Errors
///
/// [`HubError::Internal`] if the link isn't a header value, which a
/// percent-encoded URL always is: a listing without its next page would
/// silently look complete.
pub(crate) fn next_link(
    cfg: &HubConfig,
    path: &str,
    query: Option<&str>,
    next: &str,
) -> Result<HeaderValue, HubError> {
    let mut url = cfg.public_url.clone();
    url.set_path(path);
    let kept: Vec<(String, String)> = url::form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .filter(|(key, _)| key != "cursor")
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(kept)
        .append_pair("cursor", &URL_SAFE_NO_PAD.encode(next));
    HeaderValue::from_str(&format!("<{url}>; rel=\"next\"")).map_err(|e| {
        HubError::Internal(format!("the Link header is not a header value: {e}").into())
    })
}

/// The key a `cursor` parameter carries, base64url-encoded (ruling
/// M2-R8).
pub(crate) fn decode_cursor(text: &str) -> Result<String, HubError> {
    URL_SAFE_NO_PAD
        .decode(text)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(|| HubError::BadRequest {
            message: "the cursor is not one this server issued".to_owned(),
            hint: "Follow the `Link: <…>; rel=\"next\"` header of the previous page as it is.",
        })
}
