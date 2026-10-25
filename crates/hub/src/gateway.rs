//! `GET|HEAD /v1/transfer/files/{repo}/{file}`: the HTTP file gateway
//! (design §5.5, ruling M2-R9), where `resolve` redirects a Xet file and
//! clients that speak plain HTTP read it: `DuckDB`, polars, browsers, and
//! `git-lfs` through the LFS `basic` download `href` (spike S4).
//!
//! The caller's own credential authorizes the read, `Bearer`, or `Basic`
//! with the token as the password (`git-lfs` sends what it holds for the host);
//! a repository the caller may not read answers exactly as an unknown one
//! (ruling M2-R6). A file is served only to the repository whose scope
//! registered it (design §5.4).
//!
//! The answer meets S4's conformance list: `HEAD`, the three `Range` forms
//! with `416` and `Content-Range: bytes */{size}` past the end, an end
//! past the file clamped, `ETag: "{sha256}"` on every answer, `If-Range`,
//! `Accept-Ranges` and `Content-Disposition`. The bytes come from the
//! reconstruction plan `openxet_cas` plans for the reconstruction routes
//! ([`openxet_cas::plan_file`]), read from the bucket's range stream and
//! decoded chunk by chunk: a transfer holds one stream block of at most
//! 1 MiB and one chunk besides its plan (ruling M1-R27).
//!
//! A `Location` from `resolve` carries `commit`, so the gateway repeats the
//! `X-Repo-Commit`, `X-Xet-Hash` and `Link` headers `huggingface_hub` reads
//! from where a same-origin redirect lands (`utils/_http.py` lines
//! 763–782 and `file_download.py` lines 1669–1689, `huggingface_hub`
//! 2.2.0).

use std::fmt::Write as _;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::{FromRequestParts, RawPathParams, RawQuery, State};
use axum::http::header::{
    ACCEPT_RANGES, AUTHORIZATION, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_RANGE, LINK, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use futures_util::StreamExt as _;
use futures_util::stream::BoxStream;
use openxet_auth::{Action, UserPrincipal};
use openxet_cas::{ByteRange, Fetch, FilePlan, FilePlanError, plan_file};
use openxet_index::{Oid, Repo, RepoId};
use openxet_protocol::{
    CHUNK_HEADER_BYTES, ProtocolError, decode_stored_chunk, hash_from_hex, hash_to_hex,
    stored_chunk_len,
};
use openxet_storage::{Bucket, Key, StorageError};

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

/// `GET|HEAD /v1/transfer/files/{repo}/{file}?name=&commit=`.
async fn gateway(
    GatewayWho(who): GatewayWho,
    State(state): State<HubState>,
    params: RawPathParams,
    RawQuery(query): RawQuery,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, HubError> {
    let repo = readable_repo(&state, &who, &param(&params, "repo")).await?;
    let file = hash_from_hex(&param(&params, "file")).map_err(|_| HubError::BadRequest {
        message: "the file is not a Xet file hash".to_owned(),
        hint: "Use the URL exactly as resolve's Location or the LFS batch href gave it.",
    })?;
    let hex = hash_to_hex(&file);
    let (mut name, mut commit) = (None, None);
    for (key, value) in url::form_urlencoded::parse(query.as_deref().unwrap_or("").as_bytes()) {
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
    let scope = format!("repo:{}", repo.id);
    let etag = match state.index.file_sha256(&scope, &file).await? {
        Some(sha256) => format!("\"{}\"", hex_of(&sha256)),
        None => format!("\"{hex}\""),
    };
    let name = name
        .as_deref()
        .and_then(|n| n.rsplit('/').find(|c| !c.is_empty()))
        .unwrap_or(&hex);
    let mut out = HeaderMap::new();
    out.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    out.insert(X_XET_HASH, header(&hex)?);
    if let Some(commit) = commit {
        out.insert(X_REPO_COMMIT, header(&commit.to_string())?);
        if let Some(links) = View::TYPED
            .into_iter()
            .find(|v| v.admits(&repo))
            .and_then(|view| xet_links(&state, view, &repo, commit, &hex))
        {
            out.insert(LINK, header(&links)?);
        }
    }
    file_headers(&mut out, &etag, name)?;

    if method == Method::HEAD {
        let record = state
            .index
            .file(&file, &scope)
            .await?
            .ok_or_else(|| not_stored(&hex, commit))?;
        out.insert(CONTENT_LENGTH, HeaderValue::from(record.size));
        return Ok((StatusCode::OK, out).into_response());
    }
    let range = requested_range(&method, &headers, &etag);
    let FilePlan { size, range, plan } =
        match plan_file(state.index.as_ref(), &scope, &file, range).await {
            Ok(plan) => plan,
            Err(FilePlanError::NotFound) => return Err(not_stored(&hex, commit)),
            Err(FilePlanError::Unsatisfiable { size }) => return Ok(unsatisfiable(size)),
            Err(FilePlanError::Inconsistent(detail)) => {
                return Err(HubError::Internal(detail.into()));
            }
            Err(FilePlanError::Index(e)) => return Err(e.into()),
        };
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
    let mut transfer = Transfer {
        bucket: Arc::clone(&state.bucket),
        fetches: plan.fetches.into_iter(),
        blocks: None,
        block: Bytes::new(),
        carry: Vec::new(),
        skip: plan.offset_into_first_range,
        remaining: len,
        file: hex,
    };
    // The first read opens before the answer starts, so a xorb the bucket
    // lacks is a `500` rather than a body cut short.
    if len > 0 {
        transfer.open_next().await?;
    }
    let body = futures_util::stream::unfold(transfer, |mut t| async move {
        t.next_piece().await.map(|piece| (piece, t))
    });
    Ok((status, out, Body::from_stream(body)).into_response())
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

/// The answer for a file the repository's scope never registered.
fn not_stored(hex: &str, commit: Option<Oid>) -> HubError {
    HubError::EntryNotFound {
        path: hex.to_owned(),
        revision: commit.map_or_else(|| "this repository".to_owned(), |c| c.to_string()),
        commit,
    }
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

/// `416` for a file of `size` bytes (RFC 9110 §15.5.17).
pub(crate) fn unsatisfiable(size: u64) -> Response {
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::try_from(format!("bytes */{size}")) {
        headers.insert(CONTENT_RANGE, value);
    }
    headers.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    (StatusCode::RANGE_NOT_SATISFIABLE, headers).into_response()
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

fn hex_of(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(2 * bytes.len()), |mut out, b| {
            // Writing to a `String` can't fail.
            let _ = write!(out, "{b:02x}");
            out
        })
}

/// Why a transfer stopped partway.
#[derive(Debug, thiserror::Error)]
enum TransferError {
    #[error("the bucket failed: {0}")]
    Storage(#[from] StorageError),
    #[error("a stored chunk is invalid: {0}")]
    Chunk(#[from] ProtocolError),
    #[error("a fetched range ends inside a chunk")]
    Truncated,
    #[error("the plan's fetches ended before the range did")]
    ShortPlan,
}

/// A file range being read: the plan's fetches in order, each streamed
/// from the bucket and decoded one chunk at a time, `skip` bytes dropped
/// from the start and the body ending after `remaining` more.
///
/// The fetches, decoded and concatenated, are the file from the first
/// planned term's first chunk ([`openxet_cas::Plan`]). At most one stream
/// block (`block`), one stored chunk (`carry`, a chunk split across
/// blocks) and one decoded chunk are held at a time.
struct Transfer {
    bucket: Arc<dyn Bucket>,
    fetches: std::vec::IntoIter<Fetch>,
    blocks: Option<BoxStream<'static, Result<Bytes, StorageError>>>,
    block: Bytes,
    carry: Vec<u8>,
    skip: u64,
    remaining: u64,
    file: String,
}

impl Transfer {
    /// Opens the next fetch's stream.
    async fn open_next(&mut self) -> Result<(), TransferError> {
        let fetch = self.fetches.next().ok_or(TransferError::ShortPlan)?;
        let stream = self
            .bucket
            .get_range_stream(&Key::Xorb(fetch.xorb), fetch.bytes.0, fetch.bytes.1)
            .await?;
        self.blocks = Some(stream.blocks);
        Ok(())
    }

    /// The next piece of the body, `None` at its end. A failure is logged
    /// and ends the body short of its `Content-Length`, so no client takes
    /// it for the whole range.
    async fn next_piece(&mut self) -> Option<Result<Bytes, std::io::Error>> {
        match self.piece().await {
            Ok(piece) => piece.map(Ok),
            Err(e) => {
                tracing::error!(
                    file = %self.file,
                    error = &e as &dyn std::error::Error,
                    "a gateway transfer failed partway"
                );
                self.remaining = 0;
                Some(Err(std::io::Error::other(e)))
            }
        }
    }

    async fn piece(&mut self) -> Result<Option<Bytes>, TransferError> {
        loop {
            if self.remaining == 0 {
                return Ok(None);
            }
            if let Some(stored) = self.take_chunk()? {
                let data = decode_stored_chunk(&stored)?;
                let len = data.len() as u64;
                if self.skip >= len {
                    self.skip -= len;
                    continue;
                }
                let start = self.skip;
                let end = len.min(start + self.remaining);
                self.skip = 0;
                self.remaining -= end - start;
                // Both lie within one chunk, at most `MAX_CHUNK_BYTES`.
                #[allow(clippy::cast_possible_truncation, reason = "within one chunk")]
                return Ok(Some(Bytes::from(data).slice(start as usize..end as usize)));
            }
            let Some(blocks) = self.blocks.as_mut() else {
                self.open_next().await?;
                continue;
            };
            match blocks.next().await {
                Some(block) => self.block = block?,
                None if self.block.is_empty() && self.carry.is_empty() => self.blocks = None,
                None => return Err(TransferError::Truncated),
            }
        }
    }

    /// The next whole stored chunk, if the buffered bytes hold one: sliced
    /// from the block when it lies inside it, else assembled in `carry`.
    fn take_chunk(&mut self) -> Result<Option<Bytes>, TransferError> {
        if self.carry.is_empty() && self.block.len() >= CHUNK_HEADER_BYTES {
            let len = stored_chunk_len(head(&self.block))?;
            if self.block.len() >= len {
                return Ok(Some(self.block.split_to(len)));
            }
        }
        if self.carry.len() < CHUNK_HEADER_BYTES {
            let take = (CHUNK_HEADER_BYTES - self.carry.len()).min(self.block.len());
            self.carry.extend_from_slice(&self.block.split_to(take));
            if self.carry.len() < CHUNK_HEADER_BYTES {
                return Ok(None);
            }
        }
        let len = stored_chunk_len(head(&self.carry))?;
        let take = (len - self.carry.len()).min(self.block.len());
        self.carry.extend_from_slice(&self.block.split_to(take));
        if self.carry.len() < len {
            return Ok(None);
        }
        Ok(Some(Bytes::from(std::mem::take(&mut self.carry))))
    }
}

/// The chunk header `bytes` starts with; the caller checked its length.
fn head(bytes: &[u8]) -> [u8; CHUNK_HEADER_BYTES] {
    let mut header = [0; CHUNK_HEADER_BYTES];
    header.copy_from_slice(&bytes[..CHUNK_HEADER_BYTES]);
    header
}

impl From<TransferError> for HubError {
    fn from(e: TransferError) -> Self {
        match e {
            TransferError::Storage(e) => e.into(),
            e => Self::Internal(Box::new(e)),
        }
    }
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
