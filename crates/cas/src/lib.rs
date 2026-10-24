//! openxet's Xet CAS HTTP API.
//!
//! [`router`] serves the CAS routes stock clients (`hf_xet`, `git-xet`,
//! `huggingface.js`) call, at the root of its origin. The wire contract is
//! `docs/research/S10-cas-wire-contract.md`; the protocol is
//! <https://huggingface.co/docs/xet/api>.
//!
//! Every response, success or error, carries an `X-Request-Id`
//! ([`X_REQUEST_ID`]) that is also a field of the request's tracing span, so
//! a failure a client logs ties back to the server's log. Every error body is
//! `{"schema":"openxet.error.v1","code","message","hint"}` ([`CasError`]).
//! Nothing from a request body or an `Authorization` header is ever logged.

#![forbid(unsafe_code)]

mod auth;
mod blocking;
mod body;
mod dedup;
mod error;
mod misc;
mod path;
mod request_id;
mod shards;
#[cfg(test)]
mod test_support;
mod xorbs;

use std::any::Any;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::header::AUTHORIZATION;
use axum::response::{IntoResponse as _, Response};
use openxet_auth::{Authenticator, UrlSigner};
use openxet_index::{Index, Visibility};
use openxet_protocol::VerifyLimits;
use openxet_storage::Bucket;
use tokio::sync::Semaphore;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::trace::TraceLayer;
use url::Url;

pub use auth::{ReadAccess, WriteAccess};
pub use error::CasError;
pub use misc::TELEMETRY_BODY_LIMIT;
pub use request_id::{RequestId, X_REQUEST_ID};

/// The CAS's configuration.
#[derive(Debug, Clone)]
pub struct CasConfig {
    /// The origin clients reach this server at, used to build signed
    /// transfer URLs. Clients append `/v1/...` to it, so the CAS is served at
    /// the origin's root (S10 "Conventions for every route").
    pub public_url: Url,
    /// Whether one scope's xorbs are visible to another (design §5.4).
    pub visibility: Visibility,
    /// The work bounds shard verification enforces.
    pub verify_limits: VerifyLimits,
    /// How often a `/v2/shards` stream repeats its last progress event
    /// while the upload's work runs. `hf_xet` fails a stream silent for its
    /// 300 s read timeout (S10 §4).
    pub heartbeat_interval: Duration,
}

impl CasConfig {
    /// The default [`CasConfig::heartbeat_interval`]: 5 s, well inside the
    /// client's 300 s read timeout.
    pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
}

/// The current time, in Unix seconds. Credentials are checked against it, so
/// tests can control time.
pub trait Clock: Send + Sync + 'static {
    /// Seconds since the Unix epoch.
    fn now(&self) -> u64;
}

/// The system's wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        // A clock before 1970 is a misconfigured host; treating it as the
        // epoch makes every CAS token read as not yet issued, not as valid.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs())
    }
}

/// What every request handler shares. Cloning is cheap: everything is behind
/// an `Arc` except the small configuration.
#[derive(Clone)]
pub struct AppState {
    /// Which xorbs, files and shards exist, per scope.
    pub index: Arc<dyn Index>,
    /// Where xorb and shard bytes live; the source of truth.
    pub bucket: Arc<dyn Bucket>,
    /// Verifies `Authorization: Bearer` credentials.
    pub auth: Arc<Authenticator>,
    /// Signs and checks transfer URLs.
    pub urls: Arc<UrlSigner>,
    /// This server's configuration.
    pub cfg: CasConfig,
    /// Bounds concurrent xorb validation, which decompresses up to 2 × the
    /// xorb's size, and shard parsing and verification (ruling M1-R8).
    pub validate: Arc<Semaphore>,
    /// Bounds concurrent shard commits, from the bucket write to the end of
    /// index registration (ruling M1-R24), including v2 uploads whose client
    /// has left. Separate from `validate`, so slow storage never holds a
    /// CPU permit.
    ///
    /// With both, shard ingest's memory per process is at most `validate`'s
    /// permits × (a body of at most 64 MiB, its parsed form and
    /// `max_referenced_xorbs` chunk tables of at most 320 KiB each), plus
    /// `commit`'s permits × (a stored shard of at most 64 MiB and its
    /// files). Uploads waiting for a permit hold only their body.
    pub commit: Arc<Semaphore>,
    /// The time credentials are checked against.
    pub clock: Arc<dyn Clock>,
}

/// Commit permits per available CPU (ruling M1-R24).
const COMMITS_PER_CPU: usize = 4;

impl AppState {
    /// State with the system clock, one validation permit per available
    /// CPU (ruling M1-R8) and four commit permits per CPU (ruling M1-R24):
    /// committing waits on I/O, not CPU, so more can run than validate.
    #[must_use]
    pub fn new(
        index: Arc<dyn Index>,
        bucket: Arc<dyn Bucket>,
        auth: Arc<Authenticator>,
        urls: Arc<UrlSigner>,
        cfg: CasConfig,
    ) -> Self {
        let permits = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        Self {
            index,
            bucket,
            auth,
            urls,
            cfg,
            validate: Arc::new(Semaphore::new(permits)),
            commit: Arc::new(Semaphore::new(permits.saturating_mul(COMMITS_PER_CPU))),
            clock: Arc::new(SystemClock),
        }
    }

    /// The same state, checking credentials against `clock` instead.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// The CAS's routes with their middleware.
///
/// - Every response carries `X-Request-Id`, which is also the `request_id`
///   field of the request's tracing span.
/// - The trace layer logs the method, the matched route template, status
///   and latency. It never logs the raw path or query (a signed URL's `sig`
///   is a credential), a header value or a body, and `Authorization` is
///   marked sensitive besides.
/// - A path no route serves answers `404`, whatever the method: `404` is how
///   stock clients learn a route doesn't exist here (S10 "Status codes and
///   retries"). A known path with an unserved method answers `405`.
/// - A handler panic answers `500 internal` with the error body and the
///   request id, never the panic message.
/// - Responses are never compressed: clients send no `Accept-Encoding`.
pub fn router(state: AppState) -> Router {
    with_middleware(
        Router::new()
            .merge(misc::routes())
            .merge(xorbs::routes())
            .merge(shards::routes())
            .merge(dedup::routes()),
        state,
    )
}

/// The response to a handler panic: `500 internal`, logged inside the
/// request's span. The panic payload is neither logged nor sent, since it
/// may hold request data.
fn panicked(_payload: Box<dyn Any + Send + 'static>) -> Response {
    CasError::internal("a request handler panicked").into_response()
}

/// `routes` with the fallbacks and middleware every CAS route gets.
fn with_middleware(routes: Router<AppState>, state: AppState) -> Router {
    routes
        .fallback(error::no_route)
        .method_not_allowed_fallback(error::method_not_allowed)
        .with_state(state)
        .layer(CatchPanicLayer::custom(panicked))
        .layer(TraceLayer::new_for_http().make_span_with(request_id::span))
        .layer(SetSensitiveRequestHeadersLayer::new([AUTHORIZATION]))
        .layer(axum::middleware::from_fn(request_id::assign))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use axum::routing::get;
    use tower::ServiceExt as _;

    use super::*;

    async fn boom() -> &'static str {
        panic!("panic-payload-marker")
    }

    #[tokio::test]
    async fn a_panicking_handler_answers_500_with_a_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let routes = Router::new().route("/panic", get(boom));
        let app = with_middleware(routes, test_support::state(dir.path(), vec![]).await);

        let response = app
            .oneshot(Request::get("/panic").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().contains_key(X_REQUEST_ID));
        assert_eq!(response.headers()["content-type"], "application/json");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["schema"], "openxet.error.v1");
        assert_eq!(json["code"], "internal");
        assert!(!body.contains("panic-payload-marker"), "{body}");
    }
}
