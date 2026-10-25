//! `GET|HEAD /{prefix}{ns}/{name}/resolve/{rev}/{path}`: a file at a
//! revision, as `hf_hub_download`, `snapshot_download` and plain HTTP
//! clients read it (design §6, S3 "resolve" and lesson 3, S4).
//!
//! - A file stored in Git answers `200` inline with its blob's `ETag`,
//!   `X-Repo-Commit` and huggingface.co's `Content-Disposition`, ranges
//!   included (S4 G3–G9).
//! - A Xet file answers `302` to the file gateway ([`crate::gateway`],
//!   ruling M2-R9) with the headers `huggingface_hub` reads: `X-Repo-Commit`,
//!   `X-Linked-Size`, `X-Linked-ETag`, `X-Xet-Hash` and the `xet-auth` and
//!   `xet-reconstruction-info` links (`file_download.py` lines 1669–1689,
//!   `utils/_xet.py` lines 50–82, `huggingface_hub` 2.2.0). All are
//!   absolute URLs on the server's own origin (S3 lesson 3).
//! - A path that names no file answers `EntryNotFound` with `X-Repo-Commit`,
//!   which the client caches the miss under (`file_download.py` line 1773).
//!
//! The revision resolves on a request snapshot, and the path is read inside
//! that commit's tree only (Review Focus 4): `.` and `..` components, a
//! folder or the root name no file.

use axum::body::{Body, Bytes};
use axum::extract::{RawPathParams, State};
use axum::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use bstr::ByteSlice as _;
use openxet_cas::{ByteRange, CasError, Reservation, SendDeadline, send_body};
use openxet_git::{Blob, Pointer};
use openxet_index::Oid;

use crate::authz::RepoAccess;
use crate::error::X_REPO_COMMIT;
use crate::gateway::{
    X_XET_HASH, file_headers, file_url, header, requested_range, unsatisfiable, xet_links,
};
use crate::refs::at_revision;
use crate::routes::{Routes, on};
use crate::tree::xet_hash;
use crate::{HubError, HubState, param};

/// `X-Linked-Size` and `X-Linked-ETag`: the size and SHA-256 of the file a
/// redirect leads to (S3 fixture `27-…-01`).
const X_LINKED_SIZE: HeaderName = HeaderName::from_static("x-linked-size");
const X_LINKED_ETAG: HeaderName = HeaderName::from_static("x-linked-etag");

/// The resolve routes: the model view without a prefix, the other Hub
/// types under theirs (`REPO_TYPES_URL_PREFIXES`, `huggingface_hub`
/// 2.2.0).
pub(crate) fn routes() -> Routes {
    // A catch-all matches no empty path, so the root, which names no
    // file, has its own route to answer `EntryNotFound` from.
    [
        "/{ns}/{name}/resolve/{rev}/{*path}",
        "/{ns}/{name}/resolve/{rev}/",
        "/datasets/{ns}/{name}/resolve/{rev}/{*path}",
        "/datasets/{ns}/{name}/resolve/{rev}/",
        "/spaces/{ns}/{name}/resolve/{rev}/{*path}",
        "/spaces/{ns}/{name}/resolve/{rev}/",
        "/kernels/{ns}/{name}/resolve/{rev}/{*path}",
        "/kernels/{ns}/{name}/resolve/{rev}/",
    ]
    .into_iter()
    .fold(Routes::new(), |routes, path| {
        routes.route(path, on(Method::GET, resolve).private())
    })
}

async fn resolve(
    access: RepoAccess,
    State(state): State<HubState>,
    params: RawPathParams,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, HubError> {
    let (rev, path) = (param(&params, "rev"), param(&params, "path"));
    let (snap, resolved) = at_revision(&state, &access, &rev).await?;
    // The commit is `resolve_revision`'s answer on the same snapshot, as
    // `read_blob` requires (ADR 0004 item 6).
    let commit = resolved.commit;
    let at = |e| HubError::at(e, &rev, Some(commit));
    // `read_blob` buffers the whole blob, so its size, read from the object
    // header, is reserved before it is inflated (ruling M1-R40); a blob
    // past one answer's share isn't served. Streaming large plain blobs is
    // <https://github.com/stubbi/openxet/issues/91>.
    let size = state
        .engine
        .blob_size(&snap, commit, &path)
        .await
        .map_err(at)?;
    let reserved = state
        .memory
        .budget
        .reserve_answer(size)
        .await
        .map_err(|e| match e {
            CasError::AnswerTooLarge { limit, .. } => HubError::BlobTooLarge {
                path: path.clone(),
                size,
                limit,
            },
            e => e.into(),
        })?;
    let blob = state
        .engine
        .read_blob(&snap, commit, &path)
        .await
        .map_err(at)?;
    let name = path.rsplit('/').find(|c| !c.is_empty()).unwrap_or_default();
    match blob {
        Blob::Inline { oid, bytes } => {
            let held = Held {
                reserved,
                deadline: state.memory.send_deadline,
            };
            inline(commit, oid, bytes, name, &method, &headers, held)
        }
        Blob::Lfs { pointer, .. } => {
            redirect(&state, &access, &rev, &path, name, commit, &pointer).await
        }
    }
}

/// A blob's memory reservation, held until its body is sent, dropped or
/// past its send deadline.
struct Held {
    reserved: Reservation,
    deadline: SendDeadline,
}

impl Held {
    fn body(self, bytes: Bytes) -> Body {
        let deadline = self.deadline.for_len(bytes.len() as u64);
        send_body(vec![bytes], self.reserved, deadline)
    }
}

/// `200` with the Git blob `oid`'s `bytes`, or `206` with the range asked
/// for, as huggingface.co answers a small file (S3 fixture `26`).
fn inline(
    commit: Oid,
    oid: Oid,
    bytes: Bytes,
    name: &str,
    method: &Method,
    headers: &HeaderMap,
    held: Held,
) -> Result<Response, HubError> {
    let mut out = HeaderMap::new();
    out.insert(X_REPO_COMMIT, header(&commit.to_string())?);
    // huggingface.co labels every inline file `text/plain` (S3 fixtures
    // `26`, `29`, `43`); bytes that aren't UTF-8 aren't text.
    let kind = if bytes.to_str().is_ok() {
        "text/plain; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    out.insert(CONTENT_TYPE, HeaderValue::from_static(kind));
    let etag = format!("\"{oid}\"");
    file_headers(&mut out, &etag, name)?;
    let size = bytes.len() as u64;
    let part = match requested_range(method, headers, &etag) {
        None => None,
        Some(range) => match satisfiable(range, size) {
            None => return Ok(unsatisfiable(out, size)),
            part => part,
        },
    };
    let Some((start, end)) = part else {
        out.insert(CONTENT_LENGTH, HeaderValue::from(size));
        return Ok((StatusCode::OK, out, held.body(bytes)).into_response());
    };
    out.insert(
        CONTENT_RANGE,
        header(&format!("bytes {start}-{end}/{size}"))?,
    );
    out.insert(CONTENT_LENGTH, HeaderValue::from(end - start + 1));
    // Both lie within the blob, which is in memory.
    #[allow(clippy::cast_possible_truncation, reason = "within an in-memory blob")]
    let part = bytes.slice(start as usize..=end as usize);
    Ok((StatusCode::PARTIAL_CONTENT, out, held.body(part)).into_response())
}

/// The `302` to the gateway for the Xet file `pointer` names, as this
/// repository's scope registered it.
async fn redirect(
    state: &HubState,
    access: &RepoAccess,
    rev: &str,
    path: &str,
    name: &str,
    commit: Oid,
    pointer: &Pointer,
) -> Result<Response, HubError> {
    let scope = format!("repo:{}", access.repo.id);
    let Some(file) = xet_hash(state, &scope, pointer).await? else {
        return Err(HubError::LfsObjectMissing {
            path: path.to_owned(),
            revision: rev.to_owned(),
            commit,
        });
    };
    let mut out = HeaderMap::new();
    out.insert(
        LOCATION,
        header(&file_url(state, &access.repo, &file, name, commit))?,
    );
    out.insert(X_REPO_COMMIT, header(&commit.to_string())?);
    out.insert(X_LINKED_SIZE, HeaderValue::from(pointer.size));
    out.insert(
        X_LINKED_ETAG,
        header(&format!("\"{}\"", pointer.sha256_hex()))?,
    );
    out.insert(X_XET_HASH, header(&file)?);
    if let Some(links) = xet_links(state, access.view, &access.repo, commit, &file) {
        out.insert(axum::http::header::LINK, header(&links)?);
    }
    out.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    Ok((StatusCode::FOUND, out).into_response())
}

/// `range` of a file of `size` bytes, inclusive, its end clamped to the
/// file, or `None` if no byte of it exists (RFC 9110 §14.1.2).
fn satisfiable(range: ByteRange, size: u64) -> Option<(u64, u64)> {
    match range {
        ByteRange::From { start, end } if start < size => {
            Some((start, end.unwrap_or(u64::MAX).min(size - 1)))
        }
        ByteRange::Suffix(len) if len > 0 && size > 0 => Some((size - len.min(size), size - 1)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteRange, satisfiable};

    #[test]
    fn ranges_clamp_to_the_file() {
        let from = |start, end| ByteRange::From { start, end };
        assert_eq!(satisfiable(from(2, Some(6)), 21), Some((2, 6)));
        assert_eq!(satisfiable(from(2, Some(99)), 21), Some((2, 20)));
        assert_eq!(satisfiable(from(20, None), 21), Some((20, 20)));
        assert_eq!(satisfiable(from(21, None), 21), None);
        assert_eq!(satisfiable(from(0, None), 0), None);
        assert_eq!(satisfiable(ByteRange::Suffix(2), 21), Some((19, 20)));
        assert_eq!(satisfiable(ByteRange::Suffix(99), 21), Some((0, 20)));
        assert_eq!(satisfiable(ByteRange::Suffix(0), 21), None);
        assert_eq!(satisfiable(ByteRange::Suffix(1), 0), None);
    }
}
