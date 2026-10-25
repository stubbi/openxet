//! `GET|HEAD /v1/transfer/files/{repo}/{file}`: the HTTP file gateway
//! (design §5.5, ruling M2-R9), where `resolve` redirects a Xet file and
//! clients that speak plain HTTP read it: `DuckDB`, polars, browsers, and
//! `git-lfs` through the LFS `basic` download `href` (spike S4).
//!
//! The caller's own credential authorizes the read, `Bearer`, or `Basic`
//! with the token as the password; a `401` challenges `Basic` first, as
//! `git-lfs` sends the credential it holds for the host only after a
//! `Basic` challenge. A repository the caller may not read answers exactly
//! as an unknown one (ruling M2-R6). A file is served only to the
//! repository whose scope registered it (design §5.4).
//!
//! The answer meets S4's conformance list: `HEAD`, the three `Range` forms
//! with `416` and `Content-Range: bytes */{size}` past the end, an end
//! past the file clamped, the `ETag` on every answer, `If-Range`,
//! `Accept-Ranges` and `Content-Disposition` (the `name` query parameter,
//! else the Xet hash, as on an LFS `href`, which carries no name).
//!
//! The `ETag` is `"{sha256}"`, the SHA-256 this repository's scope
//! registered with the file: the value `resolve`'s `X-Linked-ETag`
//! carries, since `resolve` redirects only to the file it found by that
//! SHA-256. A file registered without one, reachable only by its Xet hash
//! and never through `resolve`, has `"{xet hash}"`.
//!
//! The bytes come from the reconstruction plan `openxet_cas` plans for the
//! reconstruction routes ([`openxet_cas::plan_file`]); they are read from
//! the bucket's range stream and decoded chunk by chunk, so a transfer
//! holds its fetches, one stream block of at most 1 MiB and one chunk
//! (ruling M1-R27, [`transfer`]). All of it is reserved from the budget
//! shared with the CAS and held until the body is sent or dropped, or goes
//! `hub.gateway_idle_timeout` without sending a byte (ruling M1-R40).
//!
//! A `Location` from `resolve` carries `commit`, so the gateway repeats the
//! `X-Repo-Commit`, `X-Xet-Hash` and `Link` headers `huggingface_hub` reads
//! from where a same-origin redirect lands (`utils/_http.py` lines
//! 763–782 and `file_download.py` lines 1669–1689, `huggingface_hub`
//! 2.2.0). That `X-Repo-Commit` is informational: the caller chose it, the
//! gateway checks only that it is a commit id, and it is never
//! authoritative. The `Link`'s `xet-auth` route goes through the first Hub
//! view the repository's labels admit, which may differ from the view
//! `resolve` was reached through; any view the repository admits mints
//! the same token.

mod transfer;

use std::fmt::Write as _;
use std::mem::size_of;
use std::sync::Arc;

use axum::extract::{FromRequestParts, RawPathParams, RawQuery, State};
use axum::http::header::{
    ACCEPT_RANGES, AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_RANGE, LINK, WWW_AUTHENTICATE,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use openxet_auth::{Action, UserPrincipal};
use openxet_cas::{ByteRange, CasError, Fetch, FilePlan, FilePlanError, plan_file};
use openxet_git::Pointer;
use openxet_index::{Oid, Repo, RepoId};
use openxet_protocol::{hash_from_hex, hash_to_hex};

use self::transfer::{Transfer, bucket_opener};
use crate::authz::{allowed, denied, user_for_token};
use crate::error::X_REPO_COMMIT;
use crate::routes::{Routes, on};
use crate::{HubError, HubState, View, param};

/// `X-Xet-Hash`: the file's Xet hash, from which `huggingface_hub` and
/// `hf_xet` read the file through the CAS (S3 lesson 3).
pub(crate) const X_XET_HASH: HeaderName = HeaderName::from_static("x-xet-hash");

/// The gateway route.
pub(crate) fn routes() -> Routes {
    Routes::new().route(
        "/v1/transfer/files/{repo}/{file}",
        on(Method::GET, gateway).private(),
    )
}

/// The gateway's URL for `file` of `repo`, named `name`, as `resolve`
/// read it at `commit` (ruling M2-R9).
pub(crate) fn file_url(
    state: &HubState,
    repo: &Repo,
    file: &str,
    name: &str,
    commit: Oid,
) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("name", name)
        .append_pair("commit", &commit.to_string())
        .finish();
    format!(
        "{}/v1/transfer/files/{}/{file}?{query}",
        state.cfg.public_url.as_str().trim_end_matches('/'),
        repo.id
    )
}

/// `Link` with the `xet-auth` route of `repo` through `view` at `commit`
/// and the CAS reconstruction of `file`, as huggingface.co sends it
/// (S3 fixture `27-…-01`); `None` for a view without Hub routes.
pub(crate) fn xet_links(
    state: &HubState,
    view: View,
    repo: &Repo,
    commit: Oid,
    file: &str,
) -> Option<String> {
    let api = view.api_prefix()?;
    let public = state.cfg.public_url.as_str().trim_end_matches('/');
    let cas = state.cfg.cas_url.as_str().trim_end_matches('/');
    Some(format!(
        "<{public}{api}/{}/{}/xet-read-token/{commit}>; rel=\"xet-auth\", \
         <{cas}/v1/reconstructions/{file}>; rel=\"xet-reconstruction-info\"",
        repo.namespace, repo.name
    ))
}

/// The user a gateway request acts for: its `Authorization` credential,
/// `Bearer` or `Basic` with the token as the password (ruling M2-R9), or
/// the anonymous principal without one.
struct GatewayWho(UserPrincipal);

impl FromRequestParts<HubState> for GatewayWho {
    type Rejection = HubError;

    async fn from_request_parts(parts: &mut Parts, state: &HubState) -> Result<Self, HubError> {
        let mut values = parts.headers.get_all(AUTHORIZATION).iter();
        let value = match (values.next(), values.next()) {
            (None, _) => return Ok(Self(UserPrincipal::anonymous())),
            (Some(value), None) => value,
            (Some(_), Some(_)) => return Err(HubError::InvalidCredential),
        };
        let (scheme, rest) = value
            .to_str()
            .ok()
            .and_then(|v| v.split_once(' '))
            .ok_or(HubError::InvalidCredential)?;
        let rest = rest.trim_start_matches(' ');
        let token = if scheme.eq_ignore_ascii_case("bearer") {
            rest.to_owned()
        } else if scheme.eq_ignore_ascii_case("basic") {
            // `user:password` (RFC 7617); the user is whatever git
            // configured, so only the password counts.
            base64::engine::general_purpose::STANDARD
                .decode(rest)
                .ok()
                .and_then(|pair| String::from_utf8(pair).ok())
                .and_then(|pair| {
                    pair.split_once(':')
                        .map(|(_, password)| password.to_owned())
                })
                .ok_or(HubError::InvalidCredential)?
        } else {
            return Err(HubError::InvalidCredential);
        };
        user_for_token(state, &token).map(Self)
    }
}

/// `GET|HEAD /v1/transfer/files/{repo}/{file}?name=&commit=`. A `401`
/// offers `Basic` before `Bearer` ([`challenge`]).
async fn gateway(
    who: Result<GatewayWho, HubError>,
    State(state): State<HubState>,
    params: RawPathParams,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Response {
    let answer = match who {
        Ok(GatewayWho(who)) => {
            serve(&who, &state, &params, query.as_deref(), &method, &headers).await
        }
        Err(e) => Err(e),
    };
    answer.unwrap_or_else(|e| challenge(e.into_response()))
}

/// `response` with a `401`'s challenge offering `Basic` first, then
/// `Bearer`: `git-lfs` sends its credential only after a `Basic`
/// challenge, and clients of `Bearer` read either.
fn challenge(mut response: Response) -> Response {
    if response.status() == StatusCode::UNAUTHORIZED {
        let headers = response.headers_mut();
        let bearer = headers.remove(WWW_AUTHENTICATE);
        headers.append(
            WWW_AUTHENTICATE,
            HeaderValue::from_static(r#"Basic realm="openxet""#),
        );
        if let Some(bearer) = bearer {
            headers.append(WWW_AUTHENTICATE, bearer);
        }
    }
    response
}

async fn serve(
    who: &UserPrincipal,
    state: &HubState,
    params: &RawPathParams,
    query: Option<&str>,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Response, HubError> {
    let repo = readable_repo(state, who, &param(params, "repo")).await?;
    let file = hash_from_hex(&param(params, "file")).map_err(|_| HubError::BadRequest {
        message: "the file is not a Xet file hash".to_owned(),
        hint: "Use the URL exactly as resolve's Location or the LFS batch href gave it.",
    })?;
    let hex = hash_to_hex(&file);
    let (name, commit) = name_and_commit(query)?;
    let scope = format!("repo:{}", repo.id);
    // One row, no terms decoded: all a `HEAD` reads.
    let summary = state
        .index
        .file_summary(&scope, &file)
        .await?
        .ok_or_else(|| HubError::FileNotStored { file: hex.clone() })?;
    let etag = match summary.sha256 {
        Some(sha256) => format!(
            "\"{}\"",
            Pointer {
                sha256,
                size: summary.size
            }
            .sha256_hex()
        ),
        None => format!("\"{hex}\""),
    };
    let name = name
        .as_deref()
        .and_then(|n| n.rsplit('/').find(|c| !c.is_empty()))
        .unwrap_or(&hex);
    let mut out = hub_headers(state, &repo, &hex, commit)?;
    file_headers(&mut out, &etag, name)?;

    if method == Method::HEAD {
        out.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
        out.insert(CONTENT_LENGTH, HeaderValue::from(summary.size));
        return Ok((StatusCode::OK, out).into_response());
    }
    let range = requested_range(method, headers, &etag);
    let budget = &state.memory.budget;
    let FilePlan {
        size,
        range,
        plan,
        mut reserved,
    } = match plan_file(state.index.as_ref(), budget, &scope, &file, range).await {
        Ok(plan) => plan,
        Err(FilePlanError::Unsatisfiable { size }) => return Ok(unsatisfiable(out, size)),
        Err(e) => return Err(plan_refused(e, hex)),
    };
    out.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    let (status, len) = match range {
        Some((start, end)) => {
            out.insert(
                CONTENT_RANGE,
                header(&format!("bytes {start}-{end}/{size}"))?,
            );
            (StatusCode::PARTIAL_CONTENT, end - start + 1)
        }
        None => (StatusCode::OK, size),
    };
    out.insert(CONTENT_LENGTH, HeaderValue::from(len));
    // The transfer keeps the fetches, one stream block, one stored chunk
    // and one decoded chunk: the reservation shrinks to the fetches and
    // takes the working bytes besides.
    let mut fetches = plan.fetches;
    fetches.shrink_to_fit();
    reserved.shrink_to((fetches.capacity() * size_of::<Fetch>()) as u64);
    if len > 0 {
        reserved.merge(
            state
                .memory
                .budget
                .reserve_answer(transfer::WORKING_BYTES)
                .await?,
        );
    }
    let mut transfer = Transfer::new(
        fetches,
        bucket_opener(Arc::clone(&state.bucket)),
        plan.offset_into_first_range,
        len,
        hex,
        Some(reserved),
    );
    // The first read opens before the answer starts, so a xorb the bucket
    // lacks is a `500` rather than a body cut short.
    if len > 0 {
        transfer.open_next().await?;
    }
    let idle = state.cfg.gateway_idle_timeout;
    Ok((status, out, transfer::body(transfer, idle)).into_response())
}

/// The `name` and `commit` query parameters.
///
/// # Errors
///
/// [`HubError::BadRequest`] for a `commit` that isn't a commit id.
fn name_and_commit(query: Option<&str>) -> Result<(Option<String>, Option<Oid>), HubError> {
    let (mut name, mut commit) = (None, None);
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        match &*key {
            "name" => name = Some(value.into_owned()),
            "commit" => {
                commit = Some(value.parse::<Oid>().map_err(|_| HubError::BadRequest {
                    message: "commit is not a 40-character commit id".to_owned(),
                    hint: "Use the URL exactly as resolve's Location gave it.",
                })?);
            }
            _ => {}
        }
    }
    Ok((name, commit))
}

/// `X-Xet-Hash`, and with a `commit` the `X-Repo-Commit` and `Link`
/// headers `huggingface_hub` reads after the redirect.
fn hub_headers(
    state: &HubState,
    repo: &Repo,
    hex: &str,
    commit: Option<Oid>,
) -> Result<HeaderMap, HubError> {
    let mut out = HeaderMap::new();
    out.insert(X_XET_HASH, header(hex)?);
    if let Some(commit) = commit {
        out.insert(X_REPO_COMMIT, header(&commit.to_string())?);
        if let Some(links) = View::TYPED
            .into_iter()
            .find(|v| v.admits(repo))
            .and_then(|view| xet_links(state, view, repo, commit, hex))
        {
            out.insert(LINK, header(&links)?);
        }
    }
    Ok(out)
}

/// The answer to a plan `plan_file` refused for the file `hex`, other than
/// an unsatisfiable range.
fn plan_refused(e: FilePlanError, hex: String) -> HubError {
    match e {
        FilePlanError::NotFound => HubError::FileNotStored { file: hex },
        FilePlanError::Memory(CasError::AnswerTooLarge { needed, limit }) => {
            HubError::AnswerTooLarge {
                needed,
                limit,
                range_helps: true,
            }
        }
        FilePlanError::RecordTooLarge { needed, limit } => HubError::AnswerTooLarge {
            needed,
            limit,
            range_helps: false,
        },
        FilePlanError::Memory(e) => e.into(),
        FilePlanError::Inconsistent(detail) => HubError::Internal(detail.into()),
        FilePlanError::Index(e) => e.into(),
        FilePlanError::Unsatisfiable { size } => HubError::Internal(
            format!("an unsatisfiable range of a {size}-byte file reached plan_refused").into(),
        ),
    }
}

/// The repository with id `id` if `who` may read it; otherwise the answer
/// for one that doesn't exist (ruling M2-R6).
async fn readable_repo(state: &HubState, who: &UserPrincipal, id: &str) -> Result<Repo, HubError> {
    let Ok(id) = id.parse::<RepoId>() else {
        return Err(denied(who));
    };
    let Some(repo) = state.index.repo_by_id(id).await? else {
        return Err(denied(who));
    };
    if !allowed(state, who, Action::Read, &repo.namespace, Some(&repo.name)).await? {
        return Err(denied(who));
    }
    Ok(repo)
}

/// The range a `GET` asks for, or `None` for the whole file: no `Range`,
/// one this server ignores (another unit, several ranges, a malformed one;
/// RFC 9110 §14.2 lets a server ignore any `Range`), or an `If-Range` that
/// isn't the current `etag` (RFC 9110 §13.1.5). `HEAD` ignores `Range`
/// (RFC 9110 §14.2).
pub(crate) fn requested_range(
    method: &Method,
    headers: &HeaderMap,
    etag: &str,
) -> Option<ByteRange> {
    if method != Method::GET {
        return None;
    }
    let range = openxet_cas::requested_range(headers).ok().flatten()?;
    match headers.get(IF_RANGE) {
        Some(validator) if validator.as_bytes() != etag.as_bytes() => None,
        _ => Some(range),
    }
}

/// `416` for a file of `size` bytes (RFC 9110 §15.5.17), with the file
/// answer's other headers `out`, its `ETag` among them (S4 G8).
pub(crate) fn unsatisfiable(mut out: HeaderMap, size: u64) -> Response {
    out.remove(CONTENT_TYPE);
    out.remove(CONTENT_LENGTH);
    if let Ok(value) = HeaderValue::try_from(format!("bytes */{size}")) {
        out.insert(CONTENT_RANGE, value);
    }
    (StatusCode::RANGE_NOT_SATISFIABLE, out).into_response()
}

/// The headers every file answer carries: `ETag`, `Accept-Ranges`, and
/// `Content-Disposition` naming `name`, and a sandboxing policy, so a
/// browser never runs a repository's content on the server's origin.
pub(crate) fn file_headers(out: &mut HeaderMap, etag: &str, name: &str) -> Result<(), HubError> {
    out.insert(ETAG, header(etag)?);
    out.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    out.insert(CONTENT_DISPOSITION, header(&content_disposition(name))?);
    out.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; sandbox"),
    );
    out.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    Ok(())
}

/// `inline; filename*=UTF-8''{name}; filename="{name}";` as huggingface.co
/// writes it (S3 fixture `26`): the name percent-encoded per RFC 8187, and
/// an ASCII fallback with every other character as `_`.
pub(crate) fn content_disposition(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            // Writing to a `String` can't fail.
            let _ = write!(encoded, "%{b:02X}");
        }
    }
    let fallback: String = name
        .chars()
        .map(|c| match c {
            '"' | '\\' => '_',
            c if c == ' ' || c.is_ascii_graphic() => c,
            _ => '_',
        })
        .collect();
    format!("inline; filename*=UTF-8''{encoded}; filename=\"{fallback}\";")
}

/// `text` as a header value. Every caller passes server-built ASCII.
pub(crate) fn header(text: &str) -> Result<HeaderValue, HubError> {
    HeaderValue::try_from(text).map_err(|e| HubError::Internal(Box::new(e)))
}

#[cfg(test)]
mod tests {
    use super::content_disposition;

    #[test]
    fn content_disposition_encodes_and_falls_back() {
        assert_eq!(
            content_disposition("config.json"),
            "inline; filename*=UTF-8''config.json; filename=\"config.json\";"
        );
        assert_eq!(
            content_disposition("a \"b\"\\\r\n.txt"),
            "inline; filename*=UTF-8''a%20%22b%22%5C%0D%0A.txt; filename=\"a _b____.txt\";"
        );
        assert_eq!(
            content_disposition("é.bin"),
            "inline; filename*=UTF-8''%C3%A9.bin; filename=\"_.bin\";"
        );
    }
}
