//! `GET …/info/lfs/xet-read-token` and `GET …/info/lfs/xet-write-token`:
//! CAS tokens for Xet-native clients such as jj-xet, which chunk and upload
//! before they know any LFS oid (ADR 0005 items 1 to 6).
//!
//! The answer is the Hub token route's, body and headers, scoped to the
//! repository and its operation, never to objects. Errors are `401` for a
//! missing or bad credential, `403` for a write token to a caller who may
//! only read, and the unknown-repository `404` otherwise (item 5, ruling
//! M2-R6). A token route never creates a repository.

use axum::extract::rejection::RawPathParamsRejection;
use axum::extract::{MatchedPath, RawPathParams, State};
use axum::http::{HeaderMap, Method};
use axum::response::Response;
use openxet_auth::{Action, Mode};

use super::{Address, LfsError, who};
use crate::HubState;
use crate::authz;
use crate::routes::{Routes, on};
use crate::tokens::issue;

/// The token routes of every Git URL.
pub(super) fn routes() -> Routes {
    let mut routes = Routes::new();
    let reads = git_paths!("/info/lfs/xet-read-token");
    let writes = git_paths!("/info/lfs/xet-write-token");
    for (read, write) in reads.into_iter().zip(writes) {
        routes = routes
            .route(
                read,
                on(
                    Method::GET,
                    |state: State<HubState>, path: MatchedPath, params, headers: HeaderMap| {
                        token(state, path, params, headers, Mode::Read)
                    },
                )
                .private(),
            )
            .route(
                write,
                on(
                    Method::GET,
                    |state: State<HubState>, path: MatchedPath, params, headers: HeaderMap| {
                        token(state, path, params, headers, Mode::Write)
                    },
                )
                .private(),
            );
    }
    routes
}

/// A CAS token of `mode` for the repository, for its caller, living
/// `hub.token_ttl`. Refreshing is asking again (item 4).
async fn token(
    State(state): State<HubState>,
    path: MatchedPath,
    params: Result<RawPathParams, RawPathParamsRejection>,
    headers: HeaderMap,
    mode: Mode,
) -> Result<Response, LfsError> {
    let who = who(&state, &headers)?;
    let access = Address::of(&path, params, &who)?
        .resolve(&state, &who)
        .await?;
    if mode == Mode::Write {
        authz::require(&state, &access.who, Action::Write, &access.repo).await?;
    }
    let exp = state
        .clock
        .now()
        .saturating_add(state.cfg.token_ttl.as_secs());
    Ok(issue(&state, &access.who.subject, &access.repo, mode, exp)?.response()?)
}
