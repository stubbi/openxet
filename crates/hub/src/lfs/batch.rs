//! `POST …/info/lfs/objects/batch` and `POST …/info/lfs/objects/verify`
//! (the Git LFS batch API, ADR 0003, ADR 0005 item 10, ruling M2-R10).
//!
//! An object is the SHA-256 and size of a file. The repository has it when
//! its scope registered a file with that SHA-256 and size from an uploaded
//! shard (ADR 0005 item 7); an object it has needs no upload, and is the
//! only kind a download can name.

use axum::body::Body;
use axum::extract::rejection::RawPathParamsRejection;
use axum::extract::{MatchedPath, RawPathParams, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use openxet_auth::{Action, Mode, UserPrincipal};
use openxet_index::Repo;
use openxet_protocol::hash_to_hex;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Address, LFS_JSON, LfsError, lfs_body, lfs_url, who};
use crate::authz::{self, RepoAccess};
use crate::repos::create_repo_for_push;
use crate::routes::{Routes, on};
use crate::tokens::{self, CasToken};
use crate::{HubError, HubState};

/// The most objects one batch may name. `git-lfs` sends at most
/// `lfs.transfer.batchSize`, 100 by default; each object costs an index
/// read.
const MAX_OBJECTS: usize = 1000;

/// The largest batch body: [`MAX_OBJECTS`] objects of about a hundred
/// bytes each, and room to spare.
const BATCH_BODY_LIMIT: usize = 1 << 20;

/// The largest verify body: one object.
const VERIFY_BODY_LIMIT: usize = 8 << 10;

/// What an upload batch without `xet` answers each object (ruling
/// M2-R10): openxet stores LFS files through Xet only until server-side
/// chunking (M4), so a `basic` upload `href` would fail later instead.
const INSTALL_GIT_XET: &str = "this server stores Git LFS files with Xet: install git-xet, run \
                               `git xet install` in this repository, and push again";

/// The batch and verify routes of every Git URL.
pub(super) fn routes() -> Routes {
    let mut routes = Routes::new();
    let batches = git_paths!("/info/lfs/objects/batch");
    let verifies = git_paths!("/info/lfs/objects/verify");
    for (batch, verify) in batches.into_iter().zip(verifies) {
        routes = routes
            .route(batch, on(Method::POST, batch_route).private())
            .route(verify, on(Method::POST, verify_route).private());
    }
    routes
}

/// A batch request (the LFS batch API; S7 records `git-lfs` 3.5.1 and
/// 3.7.1's). Unknown fields, such as `ref`, are ignored.
#[derive(Deserialize)]
struct BatchRequest {
    operation: Operation,
    /// The transfer adapters the client offers. Their order is a Go map
    /// iteration in `git-lfs`, so nothing depends on it (S7).
    #[serde(default)]
    transfers: Vec<String>,
    objects: Vec<ObjectSpec>,
    #[serde(default)]
    hash_algo: Option<String>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Upload,
    Download,
}

/// An object as a request names it. The size is signed, as `git-lfs`'s
/// is, so a negative one is a per-object error, not a malformed batch.
#[derive(Deserialize)]
struct ObjectSpec {
    oid: String,
    size: i64,
}

impl ObjectSpec {
    /// Its SHA-256 and size, or the per-object `422` the batch API gives
    /// an object it can't take.
    fn parse(&self) -> Result<([u8; 32], u64), Value> {
        let invalid = |message: &str| json!({ "code": 422, "message": message });
        let size = u64::try_from(self.size).map_err(|_| invalid("the size is negative"))?;
        let sha256 = sha256_of(&self.oid)
            .ok_or_else(|| invalid("the oid is not a lowercase hex SHA-256"))?;
        Ok((sha256, size))
    }

    /// The object's entry in the answer, before its actions or error.
    fn answer(&self) -> Value {
        json!({ "oid": self.oid, "size": self.size })
    }
}

/// The SHA-256 of `oid`, 64 lowercase hex digits, as `git-lfs` writes it.
fn sha256_of(oid: &str) -> Option<[u8; 32]> {
    if oid.len() != 64 || !oid.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(oid.as_bytes().chunks_exact(2)) {
        let pair = std::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

/// `POST …/objects/batch`.
///
/// The repository is resolved before the body is read, so a caller who
/// may not read it gets the unknown-repository answer whatever it sent
/// (ruling M2-R6). Only a caller who may create in the namespace has a
/// missing repository's body read: an upload batch creates it, as the
/// first `git push` does, since `git-lfs` uploads before git pushes
/// (ruling M2-R11).
async fn batch_route(
    State(state): State<HubState>,
    path: MatchedPath,
    params: Result<RawPathParams, RawPathParamsRejection>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, LfsError> {
    let who = who(&state, &headers)?;
    let address = Address::of(&path, params, &who)?;
    let resolved = match address.resolve(&state, &who).await {
        Ok(access) => Some(access),
        Err(HubError::UnknownRepo)
            if authz::allowed(&state, &who, Action::Create, &address.namespace, None).await? =>
        {
            None
        }
        Err(e) => return Err(e.into()),
    };
    let request: BatchRequest = lfs_body(body, BATCH_BODY_LIMIT).await?;
    if request
        .hash_algo
        .as_deref()
        .is_some_and(|algo| algo != "sha256")
    {
        return Err(LfsError::HashAlgorithm);
    }
    if request.objects.len() > MAX_OBJECTS {
        return Err(HubError::BadRequest {
            message: format!(
                "the batch names {} objects; this server takes at most {MAX_OBJECTS}",
                request.objects.len()
            ),
            hint: "Lower `lfs.transfer.batchSize` (git config); git-lfs sends 100 by default.",
        }
        .into());
    }
    // Only a batch the server would answer creates the repository.
    let access = match resolved {
        Some(access) => access,
        None if request.operation == Operation::Upload => create(&state, &who, &address).await?,
        None => return Err(HubError::UnknownRepo.into()),
    };
    let answer = match request.operation {
        Operation::Download => download(&state, &access, &request.objects).await?,
        Operation::Upload => {
            authz::require(&state, &access.who, Action::Write, &access.repo).await?;
            let xet = request.transfers.iter().any(|t| t == "xet");
            upload(&state, &access, &request.objects, xet).await?
        }
    };
    Ok(lfs_json(&answer))
}

/// Creates the repository `address` names for `who`, without an initial
/// commit, carrying the label of the view it was reached through and
/// `model` through the unprefixed URL, as the Hub's default view does
/// (ruling M2-R11). A create that loses to a concurrent one reads the
/// winner's repository.
async fn create(
    state: &HubState,
    who: &UserPrincipal,
    address: &Address,
) -> Result<RepoAccess, HubError> {
    let label = address.view.label().unwrap_or("model");
    match create_repo_for_push(state, who, &address.namespace, &address.name, label).await {
        Ok(repo) => Ok(RepoAccess {
            repo,
            who: who.clone(),
            view: address.view,
        }),
        Err(HubError::NameTaken { .. }) => address.resolve(state, who).await,
        Err(e) => Err(e),
    }
}

/// The repository's scope, `repo:{id}` (design §5.4).
fn scope(repo: &Repo) -> String {
    format!("repo:{}", repo.id)
}

/// Whether `repo` has the object `sha256` of `size` bytes; with its Xet
/// file hash if so.
async fn registered(
    state: &HubState,
    repo: &Repo,
    sha256: &[u8; 32],
    size: u64,
) -> Result<Option<String>, HubError> {
    Ok(state
        .index
        .file_by_sha256(&scope(repo), sha256)
        .await?
        .filter(|file| file.size == size)
        .map(|file| hash_to_hex(&file.hash)))
}

/// A download batch's answer: always `basic` (ADR 0003 decision 3). An
/// object the repository has gets the file gateway's `href` and only
/// `X-Xet-Hash` in its header, with no `authenticated`, so `git-lfs`
/// sends the credential it holds for the host (ruling M2-R9); one it
/// doesn't have gets the object error `404`.
async fn download(
    state: &HubState,
    access: &RepoAccess,
    objects: &[ObjectSpec],
) -> Result<Value, HubError> {
    let base = state.cfg.public_url.as_str().trim_end_matches('/');
    let mut answers = Vec::with_capacity(objects.len());
    for object in objects {
        let mut answer = object.answer();
        match object.parse() {
            Err(error) => answer["error"] = error,
            Ok((sha256, size)) => match registered(state, &access.repo, &sha256, size).await? {
                Some(hash) => {
                    answer["actions"] = json!({ "download": {
                        "href": format!("{base}/v1/transfer/files/{}/{hash}", access.repo.id),
                        "header": { "X-Xet-Hash": hash },
                    }});
                }
                None => {
                    answer["error"] = json!({
                        "code": 404,
                        "message": "the object is not stored in this repository",
                    });
                }
            },
        }
        answers.push(answer);
    }
    Ok(json!({ "transfer": "basic", "objects": answers, "hash_algo": "sha256" }))
}

/// An upload batch's answer. An object the repository has gets no
/// actions. With `xet` offered, every other object gets the `xet` upload
/// action: a capability `href` for this repository's writes and the
/// batch's CAS token in the five-header casing of ADR 0005 item 10 (all
/// but `X-Xet-Hash`, which an upload doesn't know yet), and a `verify`
/// action naming the endpoint, without a credential (§5.9 req 6). No
/// object carries `authenticated`: git-lfs v3.7.1 would then send the
/// verify without its credential (`tq/verify.go`), and git-xet 0.2.1
/// never reads it. Without
/// `xet`, every other object gets the object error `501` of ruling M2-R10.
async fn upload(
    state: &HubState,
    access: &RepoAccess,
    objects: &[ObjectSpec],
    xet: bool,
) -> Result<Value, HubError> {
    let mut actions: Option<Value> = None;
    let mut answers = Vec::with_capacity(objects.len());
    for object in objects {
        let mut answer = object.answer();
        match object.parse() {
            Err(error) => answer["error"] = error,
            Ok((sha256, size)) => {
                if registered(state, &access.repo, &sha256, size)
                    .await?
                    .is_some()
                {
                    // The batch API: no actions for an object the server has.
                } else if xet {
                    let actions = match &actions {
                        Some(actions) => actions.clone(),
                        None => actions.insert(xet_actions(state, access)?).clone(),
                    };
                    answer["actions"] = actions;
                } else {
                    answer["error"] = json!({ "code": 501, "message": INSTALL_GIT_XET });
                }
            }
        }
        answers.push(answer);
    }
    let transfer = if xet { "xet" } else { "basic" };
    Ok(json!({ "transfer": transfer, "objects": answers, "hash_algo": "sha256" }))
}

/// The `upload` and `verify` actions every object of one `xet` upload
/// batch shares: one transfer session, one capability, one CAS token.
///
/// The capability lives `hub.capability_ttl`; the token `hub.token_ttl`,
/// but never past the capability (ADR 0005 item 10). `git-xet` uses the
/// token until it nears expiry, then refreshes through the `href`.
fn xet_actions(state: &HubState, access: &RepoAccess) -> Result<Value, HubError> {
    let now = state.clock.now();
    let cap_exp = now.saturating_add(state.cfg.capability_ttl.as_secs());
    let token_exp = now
        .saturating_add(state.cfg.token_ttl.as_secs())
        .min(cap_exp);
    let CasToken {
        token,
        exp,
        cas_url,
    } = tokens::issue(
        state,
        &access.who.subject,
        &access.repo,
        Mode::Write,
        token_exp,
    )?;
    let href = super::capability::href(
        state,
        &access.repo,
        &access.who.subject,
        Mode::Write,
        cap_exp,
    );
    Ok(json!({
        "upload": {
            "href": href,
            "header": {
                "X-Xet-Cas-Url": cas_url,
                "X-Xet-Access-Token": token,
                "X-Xet-Token-Expiration": exp.to_string(),
                "X-Xet-Session-Id": uuid::Uuid::now_v7().to_string(),
            },
            "expires_in": state.cfg.capability_ttl.as_secs(),
        },
        "verify": {
            "href": format!("{}/objects/verify", lfs_url(state, access)),
        },
    }))
}

/// `POST …/objects/verify`, which `git-lfs` sends after an upload: `200
/// {}` once the repository has the object, which the upload's shard
/// registered (ADR 0005 item 7), else `404`, so an upload that never
/// completed is no silent success. Verifying is part of uploading, so it
/// needs `write`.
async fn verify_route(
    State(state): State<HubState>,
    path: MatchedPath,
    params: Result<RawPathParams, RawPathParamsRejection>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, LfsError> {
    let who = who(&state, &headers)?;
    let access = Address::of(&path, params, &who)?
        .resolve(&state, &who)
        .await?;
    authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    let object: ObjectSpec = lfs_body(body, VERIFY_BODY_LIMIT).await?;
    let (sha256, size) = object.parse().map_err(|_| HubError::BadRequest {
        message: "the object is not a lowercase hex SHA-256 and a size".to_owned(),
        hint: "Send `{\"oid\": <sha256>, \"size\": <bytes>}`, as git-lfs does.",
    })?;
    if registered(&state, &access.repo, &sha256, size)
        .await?
        .is_none()
    {
        return Err(LfsError::NotUploaded);
    }
    Ok(lfs_json(&json!({})))
}

/// `value` as an LFS answer.
fn lfs_json(value: &Value) -> Response {
    (
        [(CONTENT_TYPE, HeaderValue::from_static(LFS_JSON))],
        value.to_string(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn oids_are_lowercase_hex_sha256() {
        let oid = "474f85995709fe1d8895e50599a7252daaa0cbb2dfc4078d19416656245015d3";
        let sha = sha256_of(oid).unwrap();
        assert_eq!(sha[0], 0x47);
        assert_eq!(sha[31], 0xd3);
        assert!(sha256_of(&oid.to_uppercase()).is_none());
        assert!(sha256_of(&oid[1..]).is_none());
        assert!(sha256_of(&format!("+{}", &oid[1..])).is_none());
    }
}
