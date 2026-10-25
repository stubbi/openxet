//! openxet's Hub API: the routes `huggingface_hub` and the `hf` CLI call
//! for repositories (design §6). [`routes`] are served at the root of the
//! server's origin beside the CAS; `openxet serve` mounts them
//! ([`mounted`]) with [`openxet_http::compose`].
//!
//! The wire shapes are huggingface.co's, recorded by spike S3
//! (`docs/research/S3-hub-api-shapes.md`, `compat/fixtures/hub/`).
//! Hugging Face's repo types are views of one repository through its
//! labels ([`View`], ADR 0010). Hub errors carry huggingface.co's `error`
//! field and openxet's structured fields ([`HubError`], ruling M2-R6); a
//! repository the caller may not read answers exactly as one that doesn't
//! exist ([`RepoAccess`]).

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use openxet_auth::{Authenticator, Authorizer};
#[cfg(feature = "test-support")]
pub use openxet_http::Endpoint;
use openxet_http::{Clock, Mounted, SystemClock};
use openxet_index::HubIndex;
use openxet_storage::Bucket;
use serde::de::DeserializeOwned;
use url::Url;

/// The four paths of a typed route, one per Hub type in [`View::TYPED`]'s
/// order: `/api/models{suffix}`, `/api/datasets{suffix}`, … Routes are
/// `&'static str`, so the paths are spelled out at compile time.
macro_rules! typed_paths {
    ($suffix:literal) => {
        [
            concat!("/api/models", $suffix),
            concat!("/api/datasets", $suffix),
            concat!("/api/spaces", $suffix),
            concat!("/api/kernels", $suffix),
        ]
    };
}

mod authz;
mod error;
mod labels;
mod listing;
mod misc;
mod names;
mod repos;
#[cfg(feature = "test-support")]
pub mod test_support;
mod views;
mod whoami;

pub use authz::{RepoAccess, Who, require};
pub use error::{HubError, REPO_NOT_FOUND};
pub use labels::{MAX_LABEL_BYTES, MAX_LABELS, check_label};
pub use names::{RESERVED_NAMESPACES, check_name};
pub use repos::create_repo;
pub use views::View;

/// The Hub's configuration (`[hub]`, `docs/reference/config.md`).
#[derive(Debug, Clone)]
pub struct HubConfig {
    /// The URL clients reach the Hub API at, `HF_ENDPOINT`: every URL the
    /// Hub answers with is built from it, and `RepoUrl` parses repository
    /// URLs against it (`hf_api.py` lines 545–571).
    pub public_url: Url,
    /// The CAS URL Hub tokens name (`casUrl`).
    pub cas_url: Url,
    /// Files at least this large go through Xet (ruling M2-R4).
    pub lfs_threshold: u64,
    /// The largest commit body (ruling M2-R4).
    pub max_commit_bytes: u64,
    /// How long the CAS tokens the Hub mints live.
    pub token_ttl: Duration,
    /// The most entries one listing page holds (ruling M2-R8).
    pub page_size: usize,
}

impl HubConfig {
    /// The default [`HubConfig::lfs_threshold`]: 1 MiB (ruling M2-R4).
    pub const DEFAULT_LFS_THRESHOLD: u64 = 1 << 20;
    /// The smallest [`HubConfig::lfs_threshold`]: 1 KiB.
    pub const MIN_LFS_THRESHOLD: u64 = 1 << 10;
    /// The largest [`HubConfig::lfs_threshold`]: 10 MiB.
    pub const MAX_LFS_THRESHOLD: u64 = 10 << 20;
    /// The default [`HubConfig::max_commit_bytes`]: 64 MiB (ruling M2-R4).
    pub const DEFAULT_MAX_COMMIT_BYTES: u64 = 64 << 20;
    /// The default [`HubConfig::token_ttl`]: 15 minutes.
    pub const DEFAULT_TOKEN_TTL: Duration = Duration::from_mins(15);
    /// The largest, and default, [`HubConfig::page_size`]: 1,000 (ruling
    /// M2-R8).
    pub const MAX_PAGE_SIZE: usize = 1000;
}

/// What every Hub handler shares. Cloning is cheap.
#[derive(Clone)]
pub struct HubState {
    /// Repositories, refs and the M1 file tables.
    pub index: Arc<dyn HubIndex>,
    /// Where repository records live; the source of truth.
    pub bucket: Arc<dyn Bucket>,
    /// Verifies `Authorization: Bearer` credentials.
    pub auth: Arc<Authenticator>,
    /// Decides what a user may do per repository or namespace.
    pub authz: Arc<dyn Authorizer>,
    /// This server's Hub configuration.
    pub cfg: HubConfig,
    /// The time credentials are checked against and records are stamped
    /// with.
    pub clock: Arc<dyn Clock>,
}

impl HubState {
    /// State with the system clock.
    #[must_use]
    pub fn new(
        index: Arc<dyn HubIndex>,
        bucket: Arc<dyn Bucket>,
        auth: Arc<Authenticator>,
        authz: Arc<dyn Authorizer>,
        cfg: HubConfig,
    ) -> Self {
        Self {
            index,
            bucket,
            auth,
            authz,
            cfg,
            clock: Arc::new(SystemClock),
        }
    }

    /// The same state, with `clock` instead.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// The Hub's routes are built with `openxet_http`'s route builder, the one
/// way to add a route (ruling M2-R1).
mod routes {
    pub(crate) use openxet_http::on;

    /// Hub routes under construction.
    pub(crate) type Routes = openxet_http::Routes<crate::HubState>;
}

/// The Hub's routes, without state, fallback or middleware: those come from
/// [`openxet_http::compose`], once for the whole server. Every answer of a
/// route that depends on the caller's credential is `Cache-Control:
/// no-store`.
#[must_use]
pub fn routes() -> routes::Routes {
    routes::Routes::new()
        .merge(whoami::routes())
        .merge(repos::routes())
        .merge(listing::routes())
        .merge(labels::routes())
        .merge(misc::routes())
}

/// The Hub's [`routes`] with `state`, for [`openxet_http::compose`].
#[must_use]
pub fn mounted(state: HubState) -> Mounted {
    routes().with_state(state)
}

/// Every method and path template [`routes`] serves, from the same calls
/// that build them. Only with the `test-support` feature.
#[cfg(feature = "test-support")]
#[must_use]
pub fn endpoints() -> Vec<Endpoint> {
    routes().into_endpoints()
}

/// The largest body of a control request, `repos/create` or
/// `repos/delete`: a few short strings (`hf_api.py` lines 4638, 4768).
pub(crate) const CONTROL_BODY_LIMIT: usize = 8 << 10;

/// Reads `body` as JSON of type `T`.
///
/// # Errors
///
/// [`HubError::BodyTimeout`] for a body that fell behind its deadline,
/// [`HubError::BodyTooLarge`] past `limit` bytes, and
/// [`HubError::BadRequest`] for one that isn't the JSON the route takes.
pub(crate) async fn json_body<T: DeserializeOwned>(
    body: Body,
    limit: usize,
) -> Result<T, HubError> {
    let bytes = axum::body::to_bytes(body, limit).await.map_err(|err| {
        if openxet_http::timed_out(&err).is_some() {
            HubError::BodyTimeout
        } else if std::error::Error::source(&err)
            .is_some_and(<dyn std::error::Error>::is::<http_body_util::LengthLimitError>)
        {
            HubError::BodyTooLarge { limit }
        } else {
            HubError::BadRequest {
                message: "the request body could not be read".to_owned(),
                hint: "Resend the request with a body exactly as long as its \
                           `Content-Length`.",
            }
        }
    })?;
    serde_json::from_slice(&bytes).map_err(|err| {
        // serde's other messages quote the offending value, which could be
        // anything the client sent; a missing field names only the field.
        let what = err.to_string();
        let what = if what.starts_with("missing field") {
            what.split(" at line").next().unwrap_or_default().to_owned()
        } else {
            format!(
                "it is not the expected JSON at line {} column {}",
                err.line(),
                err.column()
            )
        };
        HubError::BadRequest {
            message: format!("the request body is malformed: {what}"),
            hint: "Send the JSON body huggingface_hub sends for this route; \
                   docs/research/S3-hub-api-shapes.md lists each.",
        }
    })
}
