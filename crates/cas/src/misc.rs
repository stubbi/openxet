use axum::Router;
use axum::body::Body;
use axum::http::StatusCode;
use axum::routing::post;
use http_body_util::BodyExt as _;

use crate::{AppState, CasError, ReadAccess};

/// The largest telemetry body accepted. `hf_xet` 1.7.0 sends five keys and
/// a flat object of counters, well under 1 KiB (S10 §6).
const TELEMETRY_BODY_LIMIT: usize = 64 * 1024;

/// Routes that carry no data: telemetry.
pub(crate) fn routes() -> Router<AppState> {
    Router::new().route("/v1/telemetry", post(telemetry))
}

/// `POST /v1/telemetry`: accepted and dropped (S10 §6; non-negotiable 6, no
/// telemetry). The body is read and discarded frame by frame, never stored
/// or logged. It is read at all so the connection stays reusable: closing it
/// with unread bytes can reset it before the client reads the `204`.
async fn telemetry(_: ReadAccess, body: Body) -> Result<StatusCode, CasError> {
    let mut body = body;
    let mut read = 0usize;
    while let Some(frame) = body.frame().await {
        let Ok(frame) = frame else {
            break;
        };
        if let Some(data) = frame.data_ref() {
            read = read.saturating_add(data.len());
            if read > TELEMETRY_BODY_LIMIT {
                return Err(CasError::PayloadTooLarge {
                    limit: TELEMETRY_BODY_LIMIT,
                });
            }
        }
    }
    Ok(StatusCode::NO_CONTENT)
}
