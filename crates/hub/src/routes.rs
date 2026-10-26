//! The Hub's routes are built with `openxet_http`'s route builder, the one
//! way to add a route (ruling M2-R1), each with its OpenAPI description
//! (ruling M2-R15), from the answers below that every route of a kind
//! shares.

use axum::http::StatusCode;

pub(crate) use openxet_http::{Area, Credential, Doc, on};

/// Hub routes under construction.
pub(crate) type Routes = openxet_http::Routes<crate::HubState>;

/// The handlers of one Hub path.
pub(crate) type Handlers = openxet_http::Handlers<crate::HubState>;

/// The credential answers of a route taking an access token.
fn credential_errors(doc: Doc) -> Doc {
    doc.error(
        StatusCode::UNAUTHORIZED,
        "`missing_credential`: no credential, and what was asked needs one. \
         `invalid_credential`: the token is unknown, malformed or expired.",
    )
    .error(
        StatusCode::FORBIDDEN,
        "`cas_credential`: a CAS token, which only the CAS routes take.",
    )
    .error(
        StatusCode::SERVICE_UNAVAILABLE,
        "`authorizer_unavailable`: the authorizer could not decide. `unavailable`: the \
         index or the bucket failed. `Retry-After: 1`.",
    )
}

/// A Hub API route of `area` taking an access token, or none for what
/// `anonymous` rules allow.
pub(crate) fn api(area: Area, summary: &'static str) -> Doc {
    credential_errors(Doc::new(area, summary, Credential::AccessToken).hub_errors())
}

/// A Hub API route on one repository, which answers a repository the caller
/// may not read exactly as one that doesn't exist (ruling M2-R6).
pub(crate) fn repo(summary: &'static str) -> Doc {
    with_repo(api(Area::Hub, summary))
}

/// A route taking an access token as `Bearer` or as the password of HTTP
/// Basic, as git and `git-lfs` send it, with the Hub's error body.
pub(crate) fn basic(area: Area, summary: &'static str) -> Doc {
    credential_errors(Doc::new(area, summary, Credential::AccessTokenOrBasic).hub_errors())
}

/// A [`basic`] route on one repository, under its Git URL.
pub(crate) fn git_url(area: Area, summary: &'static str) -> Doc {
    with_repo(basic(area, summary))
}

fn with_repo(doc: Doc) -> Doc {
    doc.error(
        StatusCode::NOT_FOUND,
        "`repo_not_found`: no such repository, or one the caller may not read; the two \
         answer alike. `wrong_label`: the caller may read the repository, but it lacks \
         the label of the route's type.",
    )
}

/// The answer to a revision that names nothing.
pub(crate) fn with_revision(doc: Doc) -> Doc {
    doc.error(
        StatusCode::NOT_FOUND,
        "`revision_not_found`: the revision is no branch or tag, nor a commit one \
         contains. `X-Error-Code: RevisionNotFound`.",
    )
}

/// The answers of a route with a request body.
pub(crate) fn with_body(doc: Doc) -> Doc {
    doc.error(
        StatusCode::BAD_REQUEST,
        "`bad_request`: the body is not what the route takes.",
    )
    .error(
        StatusCode::REQUEST_TIMEOUT,
        "`body_timeout`: the body arrived too slowly.",
    )
    .error(
        StatusCode::PAYLOAD_TOO_LARGE,
        "`payload_too_large`: the body is larger than the route takes.",
    )
}

/// The answer to a write the caller may not make on a repository it may
/// read.
pub(crate) fn with_write(doc: Doc) -> Doc {
    doc.error(
        StatusCode::FORBIDDEN,
        "`forbidden`: the caller may read the repository but not write to it.",
    )
}
