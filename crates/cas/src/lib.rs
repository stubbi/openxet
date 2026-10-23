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
mod error;
mod misc;
mod request_id;

use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::header::AUTHORIZATION;
use openxet_auth::{Authenticator, UrlSigner};
use openxet_index::{Index, Visibility};
use openxet_protocol::VerifyLimits;
use openxet_storage::Bucket;
use tokio::sync::Semaphore;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::trace::TraceLayer;
use url::Url;

pub use auth::{ReadAccess, WriteAccess};
pub use error::CasError;
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
    /// xorb's size (ruling M1-R8).
    pub validate: Arc<Semaphore>,
    /// The time credentials are checked against.
    pub clock: Arc<dyn Clock>,
}

impl AppState {
    /// State with the system clock and one validation permit per available
    /// CPU (ruling M1-R8).
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
/// - The trace layer logs method, path, status and latency. It never logs a
///   query string (a signed URL's `sig` is a credential), a header value or a
///   body, and `Authorization` is marked sensitive besides.
/// - A path no route serves answers `404`, whatever the method: `404` is how
///   stock clients learn a route doesn't exist here (S10 "Status codes and
///   retries"). A known path with an unserved method answers `405`.
/// - Responses are never compressed: clients send no `Accept-Encoding`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .merge(misc::routes())
        .fallback(error::no_route)
        .method_not_allowed_fallback(error::method_not_allowed)
        .with_state(state)
        .layer(TraceLayer::new_for_http().make_span_with(request_id::span))
        .layer(SetSensitiveRequestHeadersLayer::new([AUTHORIZATION]))
        .layer(axum::middleware::from_fn(request_id::assign))
}
