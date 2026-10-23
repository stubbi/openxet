use std::error::Error as StdError;

use axum::Json;
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::{AuthError, ForbiddenReason};
use openxet_index::IndexError;
use openxet_protocol::{MerkleHash, hash_to_hex};
use openxet_storage::StorageError;
use serde::Serialize;

/// The schema of every CAS error body.
const SCHEMA: &str = "openxet.error.v1";

/// The server-side cause of a `5xx`, logged and never sent.
type Cause = Box<dyn StdError + Send + Sync>;

/// A CAS request failure. Its response is
/// `{"schema":"openxet.error.v1","code","message","hint"}` with
/// `Content-Type: application/json`. `code` is stable snake case, for agents;
/// `hint` names the next action.
///
/// No variant carries a credential. A `5xx` keeps its cause as
/// [`std::error::Error::source`]: the cause and its own sources are logged
/// under the request's id, and the body carries only a generic message.
///
/// Errors from other crates are classified in explicit `From` impls, one arm
/// per variant, so a new upstream variant is a reviewed decision.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CasError {
    /// No route serves the path. `404 not_found`, which stock clients read
    /// as "this route doesn't exist here" (S10 "Status codes and retries").
    #[error("this CAS server has no route for the request")]
    NoRoute,

    /// The path exists, but not for this method. `405 method_not_allowed`.
    #[error("the route does not accept this HTTP method")]
    MethodNotAllowed,

    /// No bearer credential was sent. `401 missing_credential`.
    #[error("no credential was presented")]
    MissingCredential,

    /// The bearer credential is unknown, malformed or untrusted.
    /// `401 invalid_credential`.
    #[error("the credential is not valid: it is unknown, malformed or not signed by a trusted key")]
    InvalidCredential,

    /// The CAS token is at or past its `exp`. `401 expired_credential`.
    #[error("the CAS token has expired")]
    ExpiredCredential,

    /// The credential is valid but only for reading, and the route writes.
    /// `403 insufficient_scope`.
    #[error("the credential is read-only and this route writes")]
    InsufficientScope,

    /// A signed transfer URL was refused. `403 forbidden`, on which `hf_xet`
    /// fetches a fresh URL (S10 "Status codes and retries").
    #[error("the signed transfer URL was refused: {0}")]
    UrlRefused(ForbiddenReason),

    /// A signed transfer URL was fetched without its `Range` header.
    /// `400 bad_range`.
    #[error("a signed transfer URL must be fetched with the `Range` header it was signed for")]
    BadRange,

    /// A shard's file references a xorb that isn't visible to the caller.
    /// `400 unknown_xorb`, which clients don't retry.
    #[error(
        "a file in the shard references xorb {}, which is not stored on this server",
        hash_to_hex(.xorb)
    )]
    UnknownXorb {
        /// The first xorb found that isn't visible.
        xorb: MerkleHash,
    },

    /// The request body exceeds the route's limit. `413 payload_too_large`.
    #[error("the request body is larger than this route accepts ({limit} bytes)")]
    PayloadTooLarge {
        /// The route's limit in bytes.
        limit: usize,
    },

    /// The index or the bucket failed in a way a retry may fix.
    /// `503 unavailable`, which clients retry.
    #[error("the server's {component} is temporarily unavailable")]
    Unavailable {
        /// `index` or `storage`.
        component: &'static str,
        /// What failed.
        #[source]
        source: Cause,
    },

    /// A bug or an inconsistency on the server. `500 internal`.
    #[error("the server failed to handle the request")]
    Internal(#[source] Cause),
}

impl CasError {
    /// A `500 internal` whose logged cause is `cause`.
    pub fn internal(cause: impl Into<Cause>) -> Self {
        Self::Internal(cause.into())
    }
}

impl From<AuthError> for CasError {
    #[allow(
        clippy::match_same_arms,
        reason = "known variants are named so each classification is reviewed; the wildcard covers future ones"
    )]
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::Missing => Self::MissingCredential,
            AuthError::Invalid => Self::InvalidCredential,
            AuthError::Expired => Self::ExpiredCredential,
            AuthError::Forbidden { reason } => Self::UrlRefused(reason),
            AuthError::BadRange => Self::BadRange,
            // Configuration and signing failures: never the caller's fault.
            AuthError::InvalidKey { .. }
            | AuthError::InvalidClaims { .. }
            | AuthError::InvalidStaticToken { .. }
            | AuthError::LifetimeOutOfRange { .. }
            | AuthError::ReversedRange { .. }
            | AuthError::KeyGeneration => Self::internal(e),
            // A variant added upstream is a server-side failure until it is
            // classified here.
            _ => Self::internal(e),
        }
    }
}

impl From<IndexError> for CasError {
    #[allow(
        clippy::match_same_arms,
        reason = "known variants are named so each classification is reviewed; the wildcard covers future ones"
    )]
    fn from(e: IndexError) -> Self {
        match e {
            IndexError::UnknownXorb { xorb } => Self::UnknownXorb { xorb },
            IndexError::Database(_) | IndexError::Migrate(_) => Self::Unavailable {
                component: "index",
                source: e.into(),
            },
            // A corrupt row or invalid input stays wrong on retry.
            IndexError::Corrupt(_) | IndexError::InvalidInput(_) => Self::internal(e),
            // A variant added upstream (or the test-only injected fault) is
            // a server-side failure until it is classified here.
            _ => Self::internal(e),
        }
    }
}

impl From<StorageError> for CasError {
    #[allow(
        clippy::match_same_arms,
        reason = "known variants are named so each classification is reviewed; the wildcard covers future ones"
    )]
    fn from(e: StorageError) -> Self {
        match e {
            // An object the index names but the bucket lacks can be listing
            // lag in an object store, so it is retryable like an I/O failure.
            StorageError::Io { .. } | StorageError::NotFound { .. } => Self::Unavailable {
                component: "storage",
                source: e.into(),
            },
            // The index and the bucket disagree, or a hash collided: a bug.
            StorageError::RangeOutOfBounds { .. } | StorageError::SizeConflict { .. } => {
                Self::internal(e)
            }
            _ => Self::internal(e),
        }
    }
}

#[derive(Serialize)]
struct Body<'a> {
    schema: &'static str,
    code: &'static str,
    message: &'a str,
    hint: &'static str,
}

/// The hint for a xorb a shard references but the server lacks (ruling
/// M1-R5): clients dedup against shards cached for up to 21 days.
const STALE_CACHE_HINT: &str = "Upload the xorb before the shard that references it. If the \
     server was reset, clear the client's Xet cache for this endpoint \
     (~/.cache/huggingface/xet or $HF_XET_CACHE) and upload again.";

const RETRY_HINT: &str = "Retry; stock clients do so automatically. If it persists, give the \
     operator the `X-Request-Id` response header: the server log records the cause under it.";

const CREDENTIAL_HINT: &str = "Send `Authorization: Bearer <token>` with a token issued for this \
     server; the token may have been revoked or signed by a key this server doesn't trust.";

impl CasError {
    /// The HTTP status, the stable code, and the hint.
    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::NoRoute => (
                StatusCode::NOT_FOUND,
                "not_found",
                "Check the endpoint: clients append `/v1/...` to the CAS URL, which has no path \
                 and no trailing slash. The routes are listed at https://huggingface.co/docs/xet/api.",
            ),
            Self::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Use a method listed in the `Allow` response header.",
            ),
            Self::MissingCredential => (
                StatusCode::UNAUTHORIZED,
                "missing_credential",
                "Send `Authorization: Bearer <token>` with a token issued for this server.",
            ),
            Self::InvalidCredential => (
                StatusCode::UNAUTHORIZED,
                "invalid_credential",
                CREDENTIAL_HINT,
            ),
            Self::ExpiredCredential => (
                StatusCode::UNAUTHORIZED,
                "expired_credential",
                "Obtain a fresh token from your token issuer and retry.",
            ),
            Self::InsufficientScope => (
                StatusCode::FORBIDDEN,
                "insufficient_scope",
                "Use a write token for this scope; read tokens can only download and query.",
            ),
            Self::UrlRefused(_) => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "Request a fresh reconstruction for new transfer URLs, and fetch each with exactly \
                 the `Range` it was signed for.",
            ),
            Self::BadRange => (
                StatusCode::BAD_REQUEST,
                "bad_range",
                "Send `Range: bytes={start}-{end}` equal to the URL's `range` query parameter.",
            ),
            Self::UnknownXorb { .. } => (StatusCode::BAD_REQUEST, "unknown_xorb", STALE_CACHE_HINT),
            Self::PayloadTooLarge { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Send a smaller body.",
            ),
            Self::Unavailable { .. } => {
                (StatusCode::SERVICE_UNAVAILABLE, "unavailable", RETRY_HINT)
            }
            Self::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "This is a server-side failure. Give the operator the `X-Request-Id` response \
                 header: the server log records the cause under it.",
            ),
        }
    }

    /// The `WWW-Authenticate` challenge of a `401` (RFC 6750 §3): a missing
    /// credential gets the bare scheme, a rejected one `invalid_token`.
    fn challenge(&self) -> Option<HeaderValue> {
        match self {
            Self::MissingCredential => Some(HeaderValue::from_static("Bearer")),
            Self::InvalidCredential | Self::ExpiredCredential => {
                Some(HeaderValue::from_static(r#"Bearer error="invalid_token""#))
            }
            _ => None,
        }
    }
}

impl IntoResponse for CasError {
    fn into_response(self) -> Response {
        let (status, code, hint) = self.parts();
        if status.is_server_error() {
            tracing::error!(code, error = &self as &dyn StdError, "request failed");
        } else {
            tracing::debug!(code, "request refused");
        }
        let message = self.to_string();
        let mut response = (
            status,
            Json(Body {
                schema: SCHEMA,
                code,
                message: &message,
                hint,
            }),
        )
            .into_response();
        if let Some(challenge) = self.challenge() {
            response.headers_mut().insert(WWW_AUTHENTICATE, challenge);
        }
        response
    }
}

/// The fallback for paths no route serves.
pub(crate) async fn no_route() -> CasError {
    CasError::NoRoute
}

/// The fallback for a served path with an unserved method. axum adds the
/// `Allow` header.
pub(crate) async fn method_not_allowed() -> CasError {
    CasError::MethodNotAllowed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::to_bytes;
    use axum::http::HeaderMap;
    use openxet_auth::ForbiddenReason;
    use openxet_protocol::{MerkleHash, hash_to_hex};
    use openxet_storage::Key;

    use super::*;

    struct Reply {
        status: StatusCode,
        headers: HeaderMap,
        body: serde_json::Value,
    }

    async fn respond(e: impl Into<CasError>) -> Reply {
        let response = e.into().into_response();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        Reply {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap(),
        }
    }

    async fn assert_maps(e: impl Into<CasError>, status: StatusCode, code: &str) -> Reply {
        let reply = respond(e).await;
        assert_eq!(reply.status, status, "{}", reply.body);
        assert_eq!(reply.body["code"], code, "{}", reply.body);
        assert_eq!(reply.body["schema"], SCHEMA);
        reply
    }

    fn io_error() -> StorageError {
        StorageError::Io {
            op: "create temp file",
            path: PathBuf::from("/srv/secret-layout/xorbs"),
            source: io::Error::other("disk full"),
        }
    }

    fn assert_no_detail(reply: &Reply, details: &[&str]) {
        let text = reply.body.to_string();
        for detail in details {
            assert!(!text.contains(detail), "leaked {detail}: {text}");
        }
    }

    #[tokio::test]
    async fn missing_credential_is_401_with_a_bare_bearer_challenge() {
        let reply = assert_maps(
            AuthError::Missing,
            StatusCode::UNAUTHORIZED,
            "missing_credential",
        )
        .await;
        assert_eq!(reply.headers[WWW_AUTHENTICATE], "Bearer");
    }

    #[tokio::test]
    async fn invalid_credential_is_401_with_invalid_token_challenge() {
        let reply = assert_maps(
            AuthError::Invalid,
            StatusCode::UNAUTHORIZED,
            "invalid_credential",
        )
        .await;
        assert_eq!(
            reply.headers[WWW_AUTHENTICATE],
            r#"Bearer error="invalid_token""#
        );
    }

    #[tokio::test]
    async fn expired_credential_is_401_with_invalid_token_challenge() {
        let reply = assert_maps(
            AuthError::Expired,
            StatusCode::UNAUTHORIZED,
            "expired_credential",
        )
        .await;
        assert_eq!(
            reply.headers[WWW_AUTHENTICATE],
            r#"Bearer error="invalid_token""#
        );
    }

    #[tokio::test]
    async fn refused_transfer_url_is_403_forbidden() {
        // hf_xet refreshes a data URL only on a 403 (S10 "Status codes and
        // retries").
        let reply = assert_maps(
            AuthError::Forbidden {
                reason: ForbiddenReason::Expired,
            },
            StatusCode::FORBIDDEN,
            "forbidden",
        )
        .await;
        assert!(reply.headers.get(WWW_AUTHENTICATE).is_none());
    }

    #[tokio::test]
    async fn transfer_without_range_is_400_bad_range() {
        assert_maps(AuthError::BadRange, StatusCode::BAD_REQUEST, "bad_range").await;
    }

    #[tokio::test]
    async fn server_side_auth_errors_are_500_internal() {
        let failures = [
            AuthError::InvalidKey { reason: "k-detail" },
            AuthError::InvalidClaims { reason: "c-detail" },
            AuthError::InvalidStaticToken {
                index: 3,
                reason: "s-detail",
            },
            AuthError::LifetimeOutOfRange {
                setting: "l-detail",
                lifetime: Duration::ZERO,
                min: Duration::ZERO,
                max: Duration::ZERO,
            },
            AuthError::ReversedRange { start: 9, end: 1 },
            AuthError::KeyGeneration,
        ];
        for failure in failures {
            let reply = assert_maps(failure, StatusCode::INTERNAL_SERVER_ERROR, "internal").await;
            assert!(reply.headers.get(WWW_AUTHENTICATE).is_none());
            assert_no_detail(&reply, &["detail", "9-1", "random"]);
        }
    }

    #[tokio::test]
    async fn unknown_xorb_is_final_400_with_the_stale_cache_hint() {
        let xorb = MerkleHash::default();
        let reply = assert_maps(
            IndexError::UnknownXorb { xorb },
            StatusCode::BAD_REQUEST,
            "unknown_xorb",
        )
        .await;
        assert!(
            reply.body["message"]
                .as_str()
                .unwrap()
                .contains(&hash_to_hex(&xorb))
        );
        assert!(
            reply.body["hint"]
                .as_str()
                .unwrap()
                .contains("~/.cache/huggingface/xet or $HF_XET_CACHE")
        );
    }

    #[tokio::test]
    async fn index_invalid_input_is_500_internal() {
        let reply = assert_maps(
            IndexError::InvalidInput("i-detail".to_owned()),
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        )
        .await;
        assert_no_detail(&reply, &["i-detail"]);
    }

    #[tokio::test]
    async fn index_corrupt_is_500_internal() {
        let reply = assert_maps(
            IndexError::Corrupt("row 17 of xorbs".to_owned()),
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        )
        .await;
        assert_no_detail(&reply, &["row 17"]);
    }

    #[tokio::test]
    async fn index_database_failure_is_503_unavailable() {
        let reply = assert_maps(
            IndexError::Database(sqlx::Error::PoolTimedOut),
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        )
        .await;
        assert_no_detail(&reply, &["pool"]);
    }

    #[tokio::test]
    async fn index_migration_failure_is_503_unavailable() {
        let reply = assert_maps(
            IndexError::Migrate(sqlx::migrate::MigrateError::VersionMissing(4242)),
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        )
        .await;
        assert_no_detail(&reply, &["4242"]);
    }

    #[tokio::test]
    async fn storage_io_failure_is_503_unavailable() {
        let reply = assert_maps(io_error(), StatusCode::SERVICE_UNAVAILABLE, "unavailable").await;
        assert_no_detail(&reply, &["secret-layout", "disk full", "temp file"]);
    }

    #[tokio::test]
    async fn storage_not_found_is_503_unavailable() {
        // The index names an object the bucket lacks. With object stores
        // that can be a transient listing lag, so it is retryable.
        let reply = assert_maps(
            StorageError::NotFound {
                key: Key::Xorb(MerkleHash::default()),
            },
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
        )
        .await;
        assert_no_detail(&reply, &["xorbs/"]);
    }

    #[tokio::test]
    async fn storage_range_out_of_bounds_is_500_internal() {
        assert_maps(
            StorageError::RangeOutOfBounds {
                key: Key::Xorb(MerkleHash::default()),
                start: 1,
                end_inclusive: 2,
                size: 1,
            },
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        )
        .await;
    }

    #[tokio::test]
    async fn storage_size_conflict_is_500_internal() {
        assert_maps(
            StorageError::SizeConflict {
                key: Key::Shard(MerkleHash::default()),
                existing: 1,
                new: 2,
            },
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
        )
        .await;
    }

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn server_error_logs_include_the_nested_cause() {
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();

        tracing::subscriber::with_default(subscriber, || {
            drop(CasError::from(IndexError::Database(sqlx::Error::PoolTimedOut)).into_response());
        });

        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(
            logs.contains(&sqlx::Error::PoolTimedOut.to_string()),
            "the sqlx cause is missing: {logs}"
        );
    }
}
