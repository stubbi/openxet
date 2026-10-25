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
use axum::http::{HeaderName, HeaderValue, Method};
use axum::response::{IntoResponse, Response};
use openxet_auth::{Action, Mode};
use serde_json::json;

use crate::authz::{self, RepoAccess};
use crate::routes::{Routes, on};
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
                on(Method::GET, |state: State<HubState>, access: RepoAccess| {
                    token(state, access, Mode::Read)
                })
                .private(),
            )
            .route(
                write,
                on(Method::GET, |state: State<HubState>, access: RepoAccess| {
                    token(state, access, Mode::Write)
                })
                .private(),
            );
    }
    routes
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
    let now = state.clock.now();
    let exp = now.saturating_add(state.cfg.token_ttl.as_secs());
    let scope = format!("repo:{}", access.repo.id);
    let token = state
        .signer
        .sign_with_tenant(
            &access.who.subject,
            &scope,
            Some(&access.repo.namespace),
            mode,
            exp,
            now,
        )
        .map_err(|e| HubError::Internal(Box::new(e)))?;
    // `hf_xet` appends `/v1/…` to the CAS URL, so it carries no trailing
    // slash, as huggingface.co's doesn't.
    let cas_url = state.cfg.cas_url.as_str().trim_end_matches('/').to_owned();
    let header =
        |text: &str| HeaderValue::try_from(text).map_err(|e| HubError::Internal(Box::new(e)));
    let headers = [
        (X_XET_CAS_URL, header(&cas_url)?),
        (X_XET_ACCESS_TOKEN, header(&token)?),
        (X_XET_TOKEN_EXPIRATION, header(&exp.to_string())?),
    ];
    let body = json!({ "accessToken": token, "exp": exp, "casUrl": cas_url });
    Ok((headers, Json(body)).into_response())
}
