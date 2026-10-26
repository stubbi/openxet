//! `GET /api/{type}s/{id}/xet-{read|write}-token/{rev}`: CAS tokens for a
//! repository (design §5.4, S3 answer 2).
//!
//! A token is scoped to the repository, `repo:{id}`, with its namespace as
//! the tenant, so it reads and registers only this repository's files. It
//! carries no revision: `hf_xet` refreshes a write token at the branch name
//! while other commits move the branch, and a refresh must still succeed
//! (ruling M1-R11, Review Focus 5). The revision, even the literal `None`
//! a PR commit sends (`utils/_xet.py` lines 244–251), is accepted and
//! ignored.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::{Action, Mode};
use openxet_index::Repo;
use serde_json::json;

use crate::authz::{self, RepoAccess};
use crate::routes::{Doc, Routes, on, repo, with_write};
use crate::{HubError, HubState};

/// The headers `huggingface_hub` reads the token from
/// (`utils/_xet.py` lines 95–97); `hf_xet` reads the body.
const X_XET_CAS_URL: HeaderName = HeaderName::from_static("x-xet-cas-url");
const X_XET_ACCESS_TOKEN: HeaderName = HeaderName::from_static("x-xet-access-token");
const X_XET_TOKEN_EXPIRATION: HeaderName = HeaderName::from_static("x-xet-token-expiration");

/// The token routes of every Hub type.
pub(crate) fn routes() -> Routes {
    let mut routes = Routes::new();
    let read = typed_paths!("/{ns}/{name}/xet-read-token/{rev}");
    let write = typed_paths!("/{ns}/{name}/xet-write-token/{rev}");
    for (read, write) in read.into_iter().zip(write) {
        routes = routes
            .route(
                read,
                on(
                    Method::GET,
                    token_doc(repo("CAS read token")),
                    |state: State<HubState>, access: RepoAccess| token(state, access, Mode::Read),
                )
                .private(),
            )
            .route(
                write,
                on(
                    Method::GET,
                    token_doc(with_write(repo("CAS write token"))),
                    |state: State<HubState>, access: RepoAccess| token(state, access, Mode::Write),
                )
                .private(),
            );
    }
    routes
}

/// `X-Xet-Access-Token`, named apart from its description: the
/// repository's credential scan reads a quoted header name followed by a
/// quoted string as a header and its value.
const ACCESS_TOKEN_HEADER: &str = "X-Xet-Access-Token";

/// The token routes' answer, for `doc`.
pub(crate) fn token_doc(doc: Doc) -> Doc {
    doc.describe(
        "A CAS token scoped to the repository and the operation, living `hub.token_ttl`. \
         A revision in the path is accepted and ignored. To refresh, repeat the request.",
    )
    .answer(
        StatusCode::OK,
        "The token, its expiry and the CAS URL, also in the `X-Xet-*` headers.",
        Some((
            "application/json",
            json!({
                "type": "object",
                "required": ["accessToken", "exp", "casUrl"],
                "properties": {
                    "accessToken": {"type": "string", "description": "The CAS token."},
                    "exp": {"type": "integer", "description": "Its expiry, Unix seconds."},
                    "casUrl": {"type": "string", "description": "The CAS to use it with."}
                }
            }),
        )),
    )
    .answer_header(StatusCode::OK, ACCESS_TOKEN_HEADER, "The CAS token.")
    .answer_header(StatusCode::OK, "X-Xet-Cas-Url", "The CAS URL.")
    .answer_header(
        StatusCode::OK,
        "X-Xet-Token-Expiration",
        "The token's expiry, Unix seconds.",
    )
}

/// A CAS token for the repository: `200 {"accessToken","exp","casUrl"}`
/// and the same three values in `X-Xet-*` headers (S3 fixtures `09-…-03`
/// and `27-…-02`). A write token needs `write` on the repository.
async fn token(
    State(state): State<HubState>,
    access: RepoAccess,
    mode: Mode,
) -> Result<Response, HubError> {
    if mode == Mode::Write {
        authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    }
    let exp = state
        .clock
        .now()
        .saturating_add(state.cfg.token_ttl.as_secs());
    issue(&state, &access.who.subject, &access.repo, mode, exp)?.response()
}

/// A CAS token for `repo`, as the token routes answer it.
pub(crate) struct CasToken {
    /// The signed token.
    pub(crate) token: String,
    /// Its expiry, Unix seconds.
    pub(crate) exp: u64,
    /// The CAS URL it is for, without a trailing slash.
    pub(crate) cas_url: String,
}

/// A CAS token of `mode` for `repo`, for `subject`, expiring at `exp`:
/// scoped to `repo:{id}` with the namespace as its tenant (design §5.4).
///
/// # Errors
///
/// [`HubError::Internal`] if the signer refuses the claims, which it does
/// only for an `exp` not after now or past the token lifetime limit.
pub(crate) fn issue(
    state: &HubState,
    subject: &str,
    repo: &Repo,
    mode: Mode,
    exp: u64,
) -> Result<CasToken, HubError> {
    let scope = format!("repo:{}", repo.id);
    let token = state
        .signer
        .sign_with_tenant(
            subject,
            &scope,
            Some(&repo.namespace),
            mode,
            exp,
            state.clock.now(),
        )
        .map_err(|e| HubError::Internal(Box::new(e)))?;
    // `hf_xet` appends `/v1/…` to the CAS URL, so it carries no trailing
    // slash, as huggingface.co's doesn't.
    let cas_url = state.cfg.cas_url.as_str().trim_end_matches('/').to_owned();
    Ok(CasToken {
        token,
        exp,
        cas_url,
    })
}

impl CasToken {
    /// The token routes' answer: the token in the body and in the three
    /// `X-Xet-*` headers, `exp` in Unix seconds (ADR 0005 item 3).
    ///
    /// # Errors
    ///
    /// [`HubError::Internal`] for a value that can't be a header, which a
    /// signed token, a URL and a number never are.
    pub(crate) fn response(self) -> Result<Response, HubError> {
        let header =
            |text: &str| HeaderValue::try_from(text).map_err(|e| HubError::Internal(Box::new(e)));
        let headers = [
            (X_XET_CAS_URL, header(&self.cas_url)?),
            (X_XET_ACCESS_TOKEN, header(&self.token)?),
            (X_XET_TOKEN_EXPIRATION, header(&self.exp.to_string())?),
        ];
        let body = json!({ "accessToken": self.token, "exp": self.exp, "casUrl": self.cas_url });
        Ok((headers, Json(body)).into_response())
    }
}
