use axum::Router;
use axum::body::Body;
use axum::body::HttpBody as _;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use axum::routing::post;
use http_body_util::BodyExt as _;

use crate::error::closing;
use crate::{AppState, CasError, ReadAccess};

/// The largest telemetry body accepted. `hf_xet` 1.7.0 sends five keys and
/// a flat object of counters, well under 1 KiB (S10 §6).
pub const TELEMETRY_BODY_LIMIT: usize = 64 * 1024;

/// Routes that carry no data: telemetry.
pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/v1/telemetry", post(telemetry))
}

/// `POST /v1/telemetry`: accepted and dropped (S10 §6; non-negotiable 6, no
/// telemetry). The body is never stored or logged.
///
/// - A declared `Content-Length` over [`TELEMETRY_BODY_LIMIT`] answers `413`
///   before any byte is read, with `Connection: close`.
/// - Otherwise the body is read and discarded frame by frame, so the
///   connection stays reusable: closing it with unread bytes can reset it
///   before the client reads the `204`.
/// - A body that can't be read in full (the client stopped short of its
///   `Content-Length`) answers `400`, never a `204`.
async fn telemetry(_: ReadAccess, mut body: Body) -> Result<StatusCode, Response> {
    let limit = TELEMETRY_BODY_LIMIT as u64;
    // hyper's size hint is exact when the request declares a Content-Length.
    if body.size_hint().lower() > limit {
        return Err(too_large());
    }
    let mut read = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| CasError::UnreadableBody.into_response())?;
        if let Some(data) = frame.data_ref() {
            read = read.saturating_add(data.len() as u64);
            if read > limit {
                return Err(too_large());
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `413` for a body left unread, closing the connection.
fn too_large() -> Response {
    closing(CasError::PayloadTooLarge {
        limit: TELEMETRY_BODY_LIMIT,
    })
}
