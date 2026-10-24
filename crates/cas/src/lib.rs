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

mod answer;
mod auth;
mod blocking;
mod body;
mod budget;
mod cache;
mod deadline;
mod dedup;
mod error;
mod health;
mod misc;
mod path;
mod plan;
mod reconstruction;
mod request_id;
mod routes;
mod send_body;
mod shards;
#[cfg(test)]
mod test_support;
mod transfer;
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
pub use budget::MemoryBudget;
pub use deadline::{BodyDeadline, SendDeadline};
pub use error::CasError;
pub use health::starting_router;
pub use misc::TELEMETRY_BODY_LIMIT;
pub use reconstruction::{MAX_BATCH_FILES, MAX_BATCH_TERMS};
pub use request_id::{RequestId, X_REQUEST_ID};
#[cfg(feature = "test-support")]
pub use routes::Endpoint;

/// The CAS's configuration.
#[derive(Debug, Clone)]
pub struct CasConfig {
    /// The origin clients reach this server at, used to build signed
    /// transfer URLs. Clients append `/v1/...` to it, so the CAS is served at
    /// the origin's root (S10 "Conventions for every route").
    pub public_url: Url,
    /// Whether one tenant's xorbs are visible to another (design §5.4).
    pub visibility: Visibility,
    /// The work bounds shard verification enforces.
    pub verify_limits: VerifyLimits,
    /// How often a `/v2/shards` stream repeats its last progress event
    /// while the upload's work runs. `hf_xet` fails a stream silent for its
    /// 300 s read timeout (S10 §4).
    pub heartbeat_interval: Duration,
    /// The pace every request body must keep.
    pub body_deadline: BodyDeadline,
    /// How long sending a dedup or reconstruction answer may take
    /// (rulings M1-R33, M1-R40).
    pub send_deadline: SendDeadline,
    /// The bytes buffered request bodies and built answers may hold at
    /// once, process-wide (ruling M1-R40; [`MemoryBudget`]).
    pub memory_budget: u64,
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
    /// Which xorbs, files and shards exist, per scope and tenant.
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
    /// Bounds concurrent xorb and shard commits, from the bucket write to
    /// the end of index registration (rulings M1-R24, M1-R40), including v2
    /// uploads whose client has left. Separate from `validate`, so slow storage never holds a
    /// CPU permit.
    ///
    /// With both, shard ingest's memory per process is at most `validate`'s
    /// permits × (a body of at most 64 MiB, its parsed form and
    /// `max_referenced_xorbs` chunk tables of at most 320 KiB each), plus
    /// `commit`'s permits × (a stored shard of at most 64 MiB and its
    /// files). Uploads waiting for a permit hold only their body.
    pub commit: Arc<Semaphore>,
    /// Bounds concurrent global dedup answers, from loading their chunk
    /// tables until their body has been sent, dropped or overrun its send
    /// deadline (rulings M1-R32, M1-R33). Separate from `validate`, so
    /// dedup queries can never starve xorb and shard validation; a quarter
    /// of the CPUs, at least one. Dedup's memory per process is at most its
    /// permits × about 150 MB (an answer of at most 16 MiB with its build;
    /// measured in S11: about 120 MB for one, about 300 MB with 2 busy).
    pub dedup: Arc<Semaphore>,
    /// Bounds the bytes buffered upload bodies and built reconstruction
    /// answers hold at once (ruling M1-R40), from
    /// [`CasConfig::memory_budget`]. Each reserves its bytes here before
    /// it takes any of the permits above.
    pub budget: MemoryBudget,
    /// The time credentials are checked against.
    pub clock: Arc<dyn Clock>,
}

/// Commit permits per available CPU (ruling M1-R24).
const COMMITS_PER_CPU: usize = 4;

/// Dedup permits for `cpus` available CPUs: a quarter, at least one
/// (ruling M1-R32).
fn dedup_permits(cpus: usize) -> usize {
    (cpus / 4).max(1)
}

impl AppState {
    /// State with the system clock, one validation permit per available
    /// CPU (ruling M1-R8), four commit permits per CPU (ruling M1-R24):
    /// committing waits on I/O, not CPU, so more can run than validate, and
    /// a dedup permit per four CPUs (ruling M1-R32).
    #[must_use]
    pub fn new(
        index: Arc<dyn Index>,
        bucket: Arc<dyn Bucket>,
        auth: Arc<Authenticator>,
        urls: Arc<UrlSigner>,
        cfg: CasConfig,
    ) -> Self {
        let permits = std::thread::available_parallelism().map_or(1, NonZeroUsize::get);
        let budget = MemoryBudget::new(cfg.memory_budget);
        Self {
            index,
            bucket,
            auth,
            urls,
            cfg,
            validate: Arc::new(Semaphore::new(permits)),
            commit: Arc::new(Semaphore::new(permits.saturating_mul(COMMITS_PER_CPU))),
            dedup: Arc::new(Semaphore::new(dedup_permits(permits))),
            budget,
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
/// - Every request body is read under [`CasConfig::body_deadline`].
/// - `GET /healthz` and `GET /readyz` answer `200` without a credential;
///   see [`starting_router`] for the answers before the server is ready.
/// - Responses are never compressed: clients send no `Accept-Encoding`.
pub fn router(state: AppState) -> Router {
    with_middleware(served().into_router(), state)
}

/// Every method and path template [`router`] serves, from the same calls
/// that build it. Only with the `test-support` feature.
#[cfg(feature = "test-support")]
#[must_use]
pub fn endpoints() -> Vec<Endpoint> {
    served().into_endpoints()
}

/// The inclusive byte range a reconstruction serves for the `Range`
/// headers in `headers` and a file of `size` bytes, as the reconstruction
/// routes compute it; an end past the file is clamped later, by planning.
/// Only with the `fuzzing` feature, for `fuzz/`.
///
/// # Errors
///
/// [`CasError::InvalidRange`] or [`CasError::RangeNotSatisfiable`], as the
/// routes answer.
#[cfg(feature = "fuzzing")]
pub fn reconstruction_range(
    headers: &axum::http::HeaderMap,
    size: u64,
) -> Result<Option<(u64, u64)>, CasError> {
    reconstruction::resolve(reconstruction::requested_range(headers)?, size)
}

/// The CAS's routes, without middleware or state.
fn served() -> routes::Routes {
    routes::Routes::new()
        .merge(health::routes())
        .merge(misc::routes())
        .merge(xorbs::routes())
        .merge(shards::routes())
        .merge(dedup::routes())
        .merge(reconstruction::routes())
        .merge(transfer::routes())
}

/// The response to a handler panic: `500 internal`, logged inside the
/// request's span. The panic payload is neither logged nor sent, since it
/// may hold request data.
fn panicked(_payload: Box<dyn Any + Send + 'static>) -> Response {
    CasError::internal("a request handler panicked").into_response()
}

/// `routes` with the fallbacks and middleware every CAS route gets.
fn with_middleware(routes: Router<AppState>, state: AppState) -> Router {
    let body_deadline = state.cfg.body_deadline;
    routes
        .fallback(error::no_route)
        .method_not_allowed_fallback(error::method_not_allowed)
        .with_state(state)
        .layer(axum::middleware::from_fn_with_state(
            body_deadline,
            deadline::limit,
        ))
        .layer(CatchPanicLayer::custom(panicked))
        .layer(TraceLayer::new_for_http().make_span_with(request_id::span))
        .layer(SetSensitiveRequestHeadersLayer::new([AUTHORIZATION]))
        .layer(axum::middleware::from_fn(request_id::assign))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::{Body, to_bytes};
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt as _;

    use super::*;

    async fn boom() -> &'static str {
        panic!("panic-payload-marker")
    }

    #[tokio::test]
    async fn a_panicking_handler_answers_500_with_a_request_id() {
        let dir = tempfile::tempdir().unwrap();
        let routes = routes::Routes::new()
            .route("/panic", routes::on(Method::GET, boom))
            .into_router();
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

    #[test]
    fn dedup_gets_a_quarter_of_the_cpus_and_at_least_one_permit() {
        for (cpus, permits) in [(1, 1), (3, 1), (4, 1), (8, 2), (16, 4), (64, 16)] {
            assert_eq!(dedup_permits(cpus), permits, "{cpus} CPUs");
        }
    }
}
