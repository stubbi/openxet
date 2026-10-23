use axum::Json;
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::AuthError;
use openxet_index::IndexError;
use openxet_storage::StorageError;
use serde::Serialize;

/// The schema of every CAS error body.
const SCHEMA: &str = "openxet.error.v1";

/// A CAS request failure. Its response is
/// `{"schema":"openxet.error.v1","code","message","hint"}` with
/// `Content-Type: application/json`. `code` is stable snake case, for agents;
/// `hint` names the next action.
///
/// No variant carries a credential, and the server-side cause of a `5xx` is
/// logged under the request's id, never sent to the client.
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

    /// The bearer credential was missing, invalid or expired. `401`.
    #[error(transparent)]
    Unauthenticated(AuthError),

    /// The credential is valid but only for reading, and the route writes.
    /// `403 insufficient_scope`.
    #[error("the credential is read-only and this route writes")]
    InsufficientScope,

    /// The request body exceeds the route's limit. `413 payload_too_large`.
    #[error("the request body is larger than this route accepts ({limit} bytes)")]
    PayloadTooLarge {
        /// The route's limit in bytes.
        limit: usize,
    },

    /// The index failed. `503 unavailable`, which clients retry.
    #[error(transparent)]
    Index(#[from] IndexError),

    /// The bucket failed. `503 unavailable`, which clients retry.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

impl From<AuthError> for CasError {
    fn from(e: AuthError) -> Self {
        Self::Unauthenticated(e)
    }
}

#[derive(Serialize)]
struct Body<'a> {
    schema: &'static str,
    code: &'static str,
    message: &'a str,
    hint: &'static str,
}

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
            Self::Unauthenticated(AuthError::Missing) => (
                StatusCode::UNAUTHORIZED,
                "missing_credential",
                "Send `Authorization: Bearer <token>` with a token issued for this server.",
            ),
            Self::Unauthenticated(AuthError::Expired) => (
                StatusCode::UNAUTHORIZED,
                "expired_credential",
                "Obtain a fresh token from your token issuer and retry.",
            ),
            Self::Unauthenticated(_) => (
                StatusCode::UNAUTHORIZED,
                "invalid_credential",
                "Send `Authorization: Bearer <token>` with a token issued for this server; the \
                 token may have been revoked or signed by a key this server doesn't trust.",
            ),
            Self::InsufficientScope => (
                StatusCode::FORBIDDEN,
                "insufficient_scope",
                "Use a write token for this scope; read tokens can only download and query.",
            ),
            Self::PayloadTooLarge { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Send a smaller body.",
            ),
            Self::Index(_) | Self::Storage(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "Retry; stock clients do so automatically. If it persists, give the operator the \
                 `X-Request-Id` response header: the server log records the cause under it.",
            ),
        }
    }

    /// The message sent to the client. Server-side failures are described
    /// generically, so paths and database details stay in the log.
    fn public_message(&self) -> String {
        match self {
            Self::Index(_) => "the server's index is temporarily unavailable".to_owned(),
            Self::Storage(_) => "the server's storage is temporarily unavailable".to_owned(),
            other => other.to_string(),
        }
    }
}

impl IntoResponse for CasError {
    fn into_response(self) -> Response {
        let (status, code, hint) = self.parts();
        if status.is_server_error() {
            tracing::error!(code, error = %self, "request failed");
        } else {
            tracing::debug!(code, "request refused");
        }
        let message = self.public_message();
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
        if status == StatusCode::UNAUTHORIZED {
            // RFC 6750 §3: a 401 names the scheme the server accepts.
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
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

    use std::path::PathBuf;

    use axum::body::to_bytes;
    use openxet_protocol::MerkleHash;
    use openxet_storage::Key;

    use super::*;

    async fn body_of(e: CasError) -> (StatusCode, serde_json::Value) {
        let response = e.into_response();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn index_and_storage_failures_are_retryable_503_without_detail() {
        let failures = [
            CasError::from(IndexError::Corrupt("row 17 of xorbs".to_owned())),
            CasError::from(StorageError::Io {
                op: "create temp file",
                path: PathBuf::from("/srv/secret-layout/xorbs"),
                source: std::io::Error::other("disk full"),
            }),
            CasError::from(StorageError::NotFound {
                key: Key::Xorb(MerkleHash::default()),
            }),
        ];
        for failure in failures {
            let (status, body) = body_of(failure).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(body["code"], "unavailable");
            let text = body.to_string();
            for detail in ["row 17", "secret-layout", "disk full", "xorbs/"] {
                assert!(!text.contains(detail), "leaked {detail}: {text}");
            }
        }
    }

    #[tokio::test]
    async fn every_auth_failure_is_401() {
        for (e, code) in [
            (AuthError::Missing, "missing_credential"),
            (AuthError::Invalid, "invalid_credential"),
            (AuthError::Expired, "expired_credential"),
        ] {
            let (status, body) = body_of(e.into()).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(body["code"], code);
        }
    }
}
