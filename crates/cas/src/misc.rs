use axum::body::Body;
use axum::body::HttpBody as _;
use axum::http::Method;
use axum::http::StatusCode;
use axum::response::Response;
use http_body_util::BodyExt as _;

use crate::body;
use crate::error::closing;
use crate::routes::{Doc, Routes, doc, on, with_body};
use crate::{CasError, ReadAccess};

/// The largest telemetry body accepted. `hf_xet` 1.7.0 sends five keys and
/// a flat object of counters, well under 1 KiB (S10 §6).
pub const TELEMETRY_BODY_LIMIT: usize = 64 * 1024;

/// Routes that carry no data: telemetry.
pub(crate) fn routes() -> Routes {
    Routes::new().route(
        "/v1/telemetry",
        on(Method::POST, telemetry_doc(), telemetry),
    )
}

fn telemetry_doc() -> Doc {
    with_body(doc("Accept and drop transfer statistics", false))
        .describe(
            "`hf_xet` reports transfer statistics here. The body is read and dropped: \
             never stored or logged.",
        )
        .body(
            "application/json",
            &serde_json::json!({"type": "object"}),
            "Whatever the client reports; up to 64 KiB.",
        )
        .answer(StatusCode::NO_CONTENT, "Accepted and dropped.", None)
        .error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "`payload_too_large`: the body is over 64 KiB.",
        )
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
        let frame = frame.map_err(|err| body::unreadable(&err))?;
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
