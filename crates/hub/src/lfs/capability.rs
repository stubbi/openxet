//! `GET /v1/capabilities/{repo id}/{op}?exp&sub&sig`: the `href` of an LFS
//! upload action, which `git-xet` calls to refresh its CAS token (ADR 0003
//! decisions 1 and 2, ADR 0005 item 10).
//!
//! The signature is the whole authorization. The route never reads an
//! `Authorization` header for anything but whether one was sent: it is
//! never used, logged or echoed, and never fails the request. A valid
//! capability answers the token routes' response, with a CAS token for its
//! repository and operation that lives `hub.token_ttl` but never past the
//! capability's own expiry, so the `href` always outlives the tokens it
//! issues.
//!
//! The capability names and signs the subject of the batch that issued
//! it, and its tokens carry that subject, so the CAS's log attributes a
//! refresh to the user who pushed, as it does the batch's first token.

use axum::extract::rejection::RawPathParamsRejection;
use axum::extract::{RawPathParams, RawQuery, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::Response;
use openxet_auth::{AuthError, CapabilityQuery, CapabilityRefusal, Mode};
use openxet_index::{Repo, RepoId};

use super::LfsError;
use crate::routes::{Area, Credential, Doc, Routes, on};
use crate::tokens::token_doc;
use crate::{HubError, HubState, param, tokens};

/// The capability route.
pub(super) fn routes() -> Routes {
    Routes::new().route(
        "/v1/capabilities/{repo}/{op}",
        on(Method::GET, capability_doc(), capability_route).private(),
    )
}

fn capability_doc() -> Doc {
    let mut doc = token_doc(
        Doc::new(
            Area::Lfs,
            "Refresh an upload's CAS token",
            Credential::SignedUrl,
        )
        .hub_errors(),
    )
    .describe(
        "The `href` of an LFS upload action, which `git-xet` calls to refresh its CAS \
         token. The signature is the whole authorization: an `Authorization` header is \
         never used, logged or echoed. The token lives `hub.token_ttl`, never past the \
         capability's `exp`, for the repository and the pusher it was issued to. `{repo}` \
         is the repository's id and `{op}` is `write`. Use the `href` exactly as given.",
    );
    for (name, schema, description) in [
        (
            "exp",
            "integer",
            "When the capability expires, Unix seconds.",
        ),
        ("sub", "string", "The subject it was issued to."),
        (
            "sig",
            "string",
            "The signature over the repository, the operation, the subject and `exp`.",
        ),
    ] {
        doc = doc.query(
            name,
            true,
            &serde_json::json!({"type": schema}),
            description,
        );
    }
    doc.error(
        StatusCode::FORBIDDEN,
        "`capability_refused`: the capability is altered, presented for another \
         repository, operation or subject, or past its `exp`; push again for a fresh one.",
    )
    .error(
        StatusCode::NOT_FOUND,
        "`repo_not_found`: the repository no longer exists.",
    )
}

/// The capability `href` for `subject` to do `op` on `repo`, valid until
/// `exp` (Unix seconds): absolute, under the Hub's public URL, signed in
/// its query.
pub(super) fn href(state: &HubState, repo: &Repo, subject: &str, op: Mode, exp: u64) -> String {
    let CapabilityQuery { exp, sig } = state.caps.sign(&scope(repo.id, subject), op, exp);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("exp", &exp.to_string())
        .append_pair("sub", subject)
        .append_pair("sig", &sig)
        .finish();
    format!(
        "{}/v1/capabilities/{}/{}?{query}",
        state.cfg.public_url.as_str().trim_end_matches('/'),
        repo.id,
        op_name(op),
    )
}

/// The string a capability for `subject` on repository `id` is signed
/// for, as the signer's scope: `repo:{id}`, a newline, the subject. The id
/// has a fixed length and a subject holds no control character, so the
/// string splits into exactly one of each (ruling M1-R19 on subjects).
fn scope(id: RepoId, subject: &str) -> String {
    format!("repo:{id}\n{subject}")
}

fn op_name(op: Mode) -> &'static str {
    match op {
        Mode::Read => "read",
        Mode::Write => "write",
    }
}

/// What a capability request presents.
struct Presented {
    id: RepoId,
    op: Mode,
    subject: String,
    query: CapabilityQuery,
}

/// The capability a request presents: its repository, operation, subject
/// and query, the first two in the one spelling [`href`] writes, else a
/// refusal that says no more than a bad signature does.
fn presented(
    params: Result<RawPathParams, RawPathParamsRejection>,
    query: Option<&str>,
) -> Result<Presented, LfsError> {
    let refused = || LfsError::Capability(CapabilityRefusal::BadSignature);
    let params = params.map_err(|_| refused())?;
    let id_text = param(&params, "repo");
    let id = id_text
        .parse::<RepoId>()
        .ok()
        .filter(|id| id.to_string() == id_text)
        .ok_or_else(refused)?;
    let op = match param(&params, "op").as_str() {
        "read" => Mode::Read,
        "write" => Mode::Write,
        _ => return Err(refused()),
    };
    let mut exp = None;
    let mut sig = None;
    let mut subject = None;
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        match &*key {
            "exp" if exp.is_none() => exp = Some(value.parse::<u64>().map_err(|_| refused())?),
            "sig" if sig.is_none() => sig = Some(value.into_owned()),
            "sub" if subject.is_none() => subject = Some(value.into_owned()),
            _ => {}
        }
    }
    let (Some(exp), Some(sig), Some(subject)) = (exp, sig, subject) else {
        return Err(refused());
    };
    Ok(Presented {
        id,
        op,
        subject,
        query: CapabilityQuery { exp, sig },
    })
}

/// `GET /v1/capabilities/{repo id}/{op}`. Every request logs one line
/// with `authorization_present`, whether it carried an `Authorization`
/// header, and nothing of the header itself (ADR 0003 decision 2).
async fn capability_route(
    State(state): State<HubState>,
    params: Result<RawPathParams, RawPathParamsRejection>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, LfsError> {
    let authorization_present = headers.contains_key(AUTHORIZATION);
    let answer = issue(&state, params, query.as_deref()).await;
    tracing::info!(
        authorization_present,
        issued = answer.is_ok(),
        "capability request"
    );
    answer
}

/// The token a capability request is answered with: the capability is
/// checked first, then its repository looked up.
async fn issue(
    state: &HubState,
    params: Result<RawPathParams, RawPathParamsRejection>,
    query: Option<&str>,
) -> Result<Response, LfsError> {
    let Presented {
        id,
        op,
        subject,
        query,
    } = presented(params, query)?;
    let now = state.clock.now();
    state
        .caps
        .verify(&scope(id, &subject), op, &query, now)
        .map_err(|e| match e {
            AuthError::CapabilityRefused { reason } => LfsError::Capability(reason),
            e => LfsError::Hub(HubError::Internal(Box::new(e))),
        })?;
    // Signed for a repository that has since been deleted.
    let repo = state
        .index
        .repo_by_id(id)
        .await
        .map_err(HubError::from)?
        .ok_or(HubError::UnknownRepo)?;
    let exp = now
        .saturating_add(state.cfg.token_ttl.as_secs())
        .min(query.exp);
    tracing::debug!(repo = %id, op = op_name(op), "capability token issued");
    Ok(tokens::issue(state, &subject, &repo, op, exp)?.response()?)
}
