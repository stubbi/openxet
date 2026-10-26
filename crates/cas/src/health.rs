//! The liveness and readiness probes, and the server's answer while it
//! starts.
//!
//! Probes need no credential: orchestrators call them unauthenticated. They
//! are the one exception to design §5.9 req 1, and are safe because each
//! answers a fixed body that names nothing stored: at most which component
//! failed, `index` or `storage`, never its cause, which is logged.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::json;
use tokio::sync::Mutex;
use tokio::time::{Instant, timeout};
use tower_http::trace::TraceLayer;

use crate::error::closing;
use crate::routes::{Area, Credential, Doc, Routes, on};
use crate::{AppState, CasError};

const JSON: [(axum::http::HeaderName, &str); 1] = [(CONTENT_TYPE, "application/json")];

/// The probes of a server that is ready.
pub(crate) fn routes() -> Routes {
    Routes::new()
        .route("/healthz", on(Method::GET, live_doc(), live))
        .route("/readyz", on(Method::GET, ready_doc(), ready))
}

fn status(value: &str) -> serde_json::Value {
    json!({
        "type": "object",
        "required": ["status"],
        "properties": {"status": {"type": "string", "enum": [value]}}
    })
}

fn live_doc() -> Doc {
    Doc::new(Area::Health, "Liveness", Credential::None).answer(
        StatusCode::OK,
        "The process is up and serving HTTP.",
        Some(("application/json", status("ok"))),
    )
}

fn ready_doc() -> Doc {
    Doc::new(Area::Health, "Readiness", Credential::None)
        .describe(
            "Ready once the index answers a read of its tables and the bucket takes a \
             write. Each component is probed at most once a second, however often this is \
             called.",
        )
        .answer(
            StatusCode::OK,
            "The index and the bucket are open and answer.",
            Some(("application/json", status("ready"))),
        )
        .error(
            StatusCode::SERVICE_UNAVAILABLE,
            "`unavailable`: the index or the bucket failed or took over a second; the \
             message names `index` or `storage`, never the cause.",
        )
}

/// `GET /healthz`: the process is up and serving HTTP.
async fn live() -> impl IntoResponse {
    (JSON, r#"{"status":"ok"}"#)
}

/// How long a readiness answer is reused. The probe is unauthenticated, so
/// however often `/readyz` is called, the index is read and the bucket
/// written at most once per interval.
const PROBE_REUSE: Duration = Duration::from_secs(1);

/// How long the index or the bucket may take to answer a probe before it
/// counts as failed: Kubernetes' default probe timeout, past which the
/// orchestrator stops waiting anyway.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// The last readiness probe. Its lock is held while a probe runs, so
/// concurrent `/readyz` calls wait for that probe and reuse its outcome.
#[derive(Clone, Default)]
pub(crate) struct Readiness(Arc<Mutex<Option<Probed>>>);

/// When a probe ran, and the component that failed, if one did.
#[derive(Clone, Copy)]
struct Probed {
    at: Instant,
    failing: Option<&'static str>,
}

/// `GET /readyz`: `200` once the index answers a read and the bucket takes
/// a write (issue #49), else `503 unavailable` naming the component that
/// failed, so a load balancer stops routing to a node with a full disk, an
/// unwritable bucket or a broken index.
async fn ready(State(state): State<AppState>) -> Result<impl IntoResponse, CasError> {
    let mut last = state.readiness.0.lock().await;
    if let Some(Probed { at, failing }) = *last
        && at.elapsed() < PROBE_REUSE
    {
        return match failing {
            None => Ok((JSON, r#"{"status":"ready"}"#)),
            Some(component) => Err(CasError::Unavailable {
                component,
                source: "the readiness probe under a second ago failed; its cause is logged \
                         with that probe"
                    .into(),
            }),
        };
    }
    let probed = probe(&state).await;
    *last = Some(Probed {
        at: Instant::now(),
        failing: probed.as_ref().err().map(|e| match e {
            CasError::Unavailable { component, .. } => *component,
            _ => unreachable!("`within` fails only as unavailable"),
        }),
    });
    probed.map(|()| (JSON, r#"{"status":"ready"}"#))
}

/// Probes the index and the bucket at once. If both fail, the bucket's
/// failure is logged here and the index's answered.
async fn probe(state: &AppState) -> Result<(), CasError> {
    let (index, bucket) = tokio::join!(
        within("index", state.index.probe()),
        within("storage", state.bucket.probe()),
    );
    match (index, bucket) {
        (Err(index), Err(bucket)) => {
            bucket.log();
            Err(index)
        }
        (index, bucket) => index.and(bucket),
    }
}

/// `probe`'s outcome as `component`'s, failed if it takes longer than
/// [`PROBE_TIMEOUT`].
async fn within<E>(
    component: &'static str,
    probe: impl Future<Output = Result<(), E>>,
) -> Result<(), CasError>
where
    E: std::error::Error + Send + Sync + 'static,
{
    let source = match timeout(PROBE_TIMEOUT, probe).await {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(e)) => e.into(),
        Err(_) => format!("no answer to the readiness probe within {PROBE_TIMEOUT:?}").into(),
    };
    Err(CasError::Unavailable { component, source })
}

/// `503 not_ready`, closing the connection: a request body is left unread.
async fn not_ready() -> Response {
    closing(CasError::NotReady)
}

/// What a server answers while its index and bucket open, before it serves
/// [`crate::routes`]: `/healthz` is `200`, and `/readyz` and every other
/// path `503 not_ready` with `Retry-After: 1`, which clients retry.
#[expect(
    clippy::disallowed_methods,
    reason = "the probes and fallback before the CAS routes exist; `routes` lists the served ones"
)]
pub fn starting_router() -> Router {
    Router::new()
        .route("/healthz", get(live))
        .route("/readyz", get(not_ready))
        .fallback(not_ready)
        .layer(TraceLayer::new_for_http().make_span_with(openxet_http::span))
        .layer(axum::middleware::from_fn(openxet_http::assign))
}

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_probe_without_an_answer_fails_after_the_timeout() {
        let started = Instant::now();
        let probed = within("index", std::future::pending::<Result<(), io::Error>>()).await;
        assert!(
            matches!(
                probed,
                Err(CasError::Unavailable {
                    component: "index",
                    ..
                })
            ),
            "{probed:?}"
        );
        assert!(started.elapsed() >= PROBE_TIMEOUT);
    }

    #[tokio::test]
    async fn a_failed_probe_names_its_component() {
        let probed = within("storage", async { Err(io::Error::other("disk full")) }).await;
        assert!(
            matches!(
                probed,
                Err(CasError::Unavailable {
                    component: "storage",
                    ..
                })
            ),
            "{probed:?}"
        );
        assert!(
            within("index", async { Ok::<_, io::Error>(()) })
                .await
                .is_ok()
        );
    }
}
