//! The CAS's routes are built with `openxet_http`'s route builder, the one
//! way to add a route (ruling M2-R1), over the CAS's [`AppState`], each
//! with its OpenAPI description (ruling M2-R15). The answers are those of
//! `docs/reference/routes.md`.

use axum::http::StatusCode;

use crate::AppState;

pub(crate) use openxet_http::{Area, Credential, Doc, on};

/// CAS routes under construction.
pub(crate) type Routes = openxet_http::Routes<AppState>;

/// A CAS route taking a CAS token that may `read`, or `write` if `write`:
/// with the answers every such route can give.
pub(crate) fn doc(summary: &'static str, write: bool) -> Doc {
    let doc = Doc::new(Area::Cas, summary, Credential::CasToken)
        .error(
            StatusCode::BAD_REQUEST,
            "`invalid_path`: the path holds a percent-encoded byte. `invalid_hash`: a hash \
             is not 64 lowercase hex characters.",
        )
        .error(
            StatusCode::UNAUTHORIZED,
            "`missing_credential`, `invalid_credential`, `expired_credential`: \
             `WWW-Authenticate` names the problem.",
        )
        .error(
            StatusCode::SERVICE_UNAVAILABLE,
            "`unavailable`: the index or bucket failed in a way a retry may fix.",
        );
    if write {
        doc.error(
            StatusCode::FORBIDDEN,
            "`insufficient_scope`: a read token on a route that writes.",
        )
    } else {
        doc
    }
}

/// [`doc`] for a route with a request body, which can also time out.
pub(crate) fn with_body(doc: Doc) -> Doc {
    doc.error(
        StatusCode::REQUEST_TIMEOUT,
        "`body_timeout`: the body stalled or fell below `server.body_min_rate`. The \
         answer closes the connection; retry.",
    )
    .error(
        StatusCode::BAD_REQUEST,
        "`bad_request`: the body ended before its `Content-Length`, or its transfer was \
         malformed.",
    )
}
