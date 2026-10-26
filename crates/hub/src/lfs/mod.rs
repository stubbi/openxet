//! The Git LFS endpoint, `{prefix}{ns}/{name}[.git]/info/lfs`, for
//! `git-lfs`, `git-xet` and Xet-native clients (design §6.1, ADRs 0003
//! and 0005).
//!
//! - `objects/batch` answers uploads that offer `xet` with a signed
//!   capability `href` and the batch's CAS token, uploads that don't with
//!   an object error naming `git-xet` (ruling M2-R10), and downloads always
//!   with `basic` through the file gateway (ADR 0003 decision 3).
//! - `xet-read-token` and `xet-write-token` hand Xet-native clients CAS
//!   tokens (ADR 0005 items 1 to 5).
//! - `/v1/capabilities/{repo id}/{op}` refreshes an upload's CAS token on
//!   the strength of its signature alone (ADR 0003 decision 2).
//!
//! Credentials are the Hub's, as `Bearer` or as HTTP Basic whose password
//! is the token, which is what git's credential helpers hand `git-lfs`
//! (ADR 0005 item 2). The unprefixed URL serves a repository whatever its
//! labels; a typed one (`datasets/…`) sees it through that label (ADR 0010
//! item 4).

mod batch;
mod capability;
mod tokens;

use axum::body::Body;
use axum::extract::rejection::RawPathParamsRejection;
use axum::extract::{MatchedPath, RawPathParams};
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::{CapabilityRefusal, UserPrincipal};
use openxet_http::{ErrorBody, HubErrorBody};
use serde::de::DeserializeOwned;

use crate::authz::{self, RepoAccess, Who};
use crate::routes::Routes;
use crate::{HubError, HubState, View};

/// The media type of the batch API's requests and answers.
const LFS_JSON: &str = "application/vnd.git-lfs+json";

/// The challenge `git-lfs` reads to ask its credential helpers for a user
/// name and password, which `huggingface.co` sends too (S7 fixture
/// `hf/02-…-02`).
const LFS_AUTHENTICATE: HeaderName = HeaderName::from_static("lfs-authenticate");

/// The `Basic` challenge of every LFS `401`.
const BASIC_CHALLENGE: &str = r#"Basic realm="openxet""#;

/// The LFS routes.
pub(crate) fn routes() -> Routes {
    batch::routes()
        .merge(tokens::routes())
        .merge(capability::routes())
}

/// The caller of an LFS request, by `Bearer` or by HTTP Basic with the
/// token as the password.
fn who(state: &HubState, headers: &HeaderMap) -> Result<UserPrincipal, HubError> {
    Who::from_headers(headers, state, true).map(|Who(who)| who)
}

/// The repository an LFS route names, as the client wrote it.
struct Address {
    namespace: String,
    /// Without the `.git` suffix.
    name: String,
    view: View,
}

impl Address {
    /// The address of the matched route `path` with `params`. Parameters
    /// that aren't UTF-8 name no repository that can exist, which answers
    /// as one that doesn't, for `who`.
    fn of(
        path: &MatchedPath,
        params: Result<RawPathParams, RawPathParamsRejection>,
        who: &UserPrincipal,
    ) -> Result<Self, HubError> {
        let params = params.map_err(|_| authz::denied(who))?;
        let namespace = crate::param(&params, "ns");
        let name = crate::param(&params, "name");
        let name = name.strip_suffix(".git").unwrap_or(&name).to_owned();
        // The unprefixed Git URL is openxet's own address (ADR 0010 item 4).
        let view = [View::Dataset, View::Space, View::Kernel]
            .into_iter()
            .find(|view| {
                path.as_str()
                    .strip_prefix('/')
                    .is_some_and(|p| p.starts_with(view.url_prefix()))
            })
            .unwrap_or(View::Any);
        Ok(Self {
            namespace,
            name,
            view,
        })
    }

    /// The repository, if `who` may read it, as every Hub route resolves
    /// one (ruling M2-R6).
    async fn resolve(&self, state: &HubState, who: &UserPrincipal) -> Result<RepoAccess, HubError> {
        RepoAccess::resolve(state, who.clone(), &self.namespace, &self.name, self.view).await
    }
}

/// `body` as the JSON of type `T` an LFS route takes, up to `limit` bytes.
///
/// # Errors
///
/// Those of [`crate::read_body`], and [`HubError::BadRequest`] for a body
/// that isn't that JSON, never quoting it.
async fn lfs_body<T: DeserializeOwned>(body: Body, limit: usize) -> Result<T, HubError> {
    let bytes = crate::read_body(body, limit).await?;
    serde_json::from_slice(&bytes).map_err(|err| HubError::BadRequest {
        message: format!(
            "the request body is not the Git LFS batch API's JSON at line {} column {}",
            err.line(),
            err.column()
        ),
        hint: "Send the JSON body git-lfs sends to this route (the Git LFS batch API).",
    })
}

/// The LFS URL of `access`'s repository, as the client reached it:
/// `{public_url}/{prefix}{ns}/{name}.git/info/lfs`.
fn lfs_url(state: &HubState, access: &RepoAccess) -> String {
    format!(
        "{}.git/info/lfs",
        access
            .view
            .repo_url(&state.cfg, &access.repo.namespace, &access.repo.name)
    )
}

/// An LFS route's failure.
#[derive(Debug)]
enum LfsError {
    /// A Hub failure, answered as the Hub answers it, but for `401`s,
    /// which ask for git's credentials.
    Hub(HubError),
    /// The batch names a hash algorithm other than SHA-256. `409`, as the
    /// batch API asks.
    HashAlgorithm,
    /// A verify for an object this repository doesn't have. `404`.
    NotUploaded,
    /// A capability `href` whose signature doesn't match, or that has
    /// expired. `403`.
    Capability(CapabilityRefusal),
}

impl From<HubError> for LfsError {
    fn from(e: HubError) -> Self {
        Self::Hub(e)
    }
}

impl LfsError {
    /// The status, code, message and hint of the answer, or the Hub
    /// failure that answers as the Hub answers it.
    fn parts(self) -> Result<(StatusCode, &'static str, &'static str, &'static str), HubError> {
        Ok(match self {
            Self::Hub(HubError::MissingCredential) => (
                StatusCode::UNAUTHORIZED,
                "missing_credential",
                "Authentication required: give git an openxet access token as the password",
                "git asks its credential helper for the password: store the token for this host \
                 with `git credential approve`, or send it as `Bearer <token>`. The server's \
                 operator issues tokens.",
            ),
            Self::Hub(HubError::InvalidCredential) => (
                StatusCode::UNAUTHORIZED,
                "invalid_credential",
                "Invalid credentials in Authorization header",
                "The token is unknown, malformed or expired. Store a token the server's \
                 operator issued as the password for this host (`git credential approve`), \
                 then retry.",
            ),
            Self::Hub(e) => return Err(e),
            Self::HashAlgorithm => (
                StatusCode::CONFLICT,
                "unsupported_hash_algorithm",
                "this server stores Git LFS objects by SHA-256 only",
                "Leave `hash_algo` out or send `sha256`, as git-lfs does by default.",
            ),
            Self::NotUploaded => (
                StatusCode::NOT_FOUND,
                "object_not_uploaded",
                "the object was not uploaded to this repository",
                "The upload did not complete: push again, and check that git-xet is installed \
                 (`git xet install`).",
            ),
            Self::Capability(CapabilityRefusal::Expired) => (
                StatusCode::FORBIDDEN,
                "capability_refused",
                "the capability has expired",
                "Push again: git-lfs asks the batch API for a fresh capability. An upload \
                 longer than `hub.capability_ttl` needs the operator to raise it.",
            ),
            // A bad signature, and any refusal a later signer adds.
            Self::Capability(_) => (
                StatusCode::FORBIDDEN,
                "capability_refused",
                "the capability is not valid",
                "Use the href of a batch answer exactly as given; push again for a fresh one.",
            ),
        })
    }
}

impl IntoResponse for LfsError {
    fn into_response(self) -> Response {
        let (status, code, message, hint) = match self.parts() {
            Ok(parts) => parts,
            Err(e) => return e.into_response(),
        };
        tracing::debug!(code, "request refused");
        let mut response =
            HubErrorBody::new(ErrorBody::new(status, code, message, hint), None).into_response();
        if status == StatusCode::UNAUTHORIZED {
            let headers = response.headers_mut();
            headers.insert(WWW_AUTHENTICATE, HeaderValue::from_static(BASIC_CHALLENGE));
            headers.insert(LFS_AUTHENTICATE, HeaderValue::from_static(BASIC_CHALLENGE));
        }
        response
    }
}
