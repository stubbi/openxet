use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use openxet_auth::{AuthError, Principal};

use crate::{AppState, CasError};

/// A caller allowed to read: any valid bearer credential.
///
/// The extractor only authenticates. Handlers check the principal's scope
/// against the data they serve (design §5.4).
#[derive(Debug, Clone)]
pub struct ReadAccess(pub Principal);

/// A caller allowed to write: a valid bearer credential in write mode. A
/// read credential answers `403 insufficient_scope`.
///
/// The extractor only authenticates. Handlers check the principal's scope
/// against the data they write (design §5.4).
#[derive(Debug, Clone)]
pub struct WriteAccess(pub Principal);

impl FromRequestParts<AppState> for ReadAccess {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        let principal = authenticate(parts, state)?;
        if principal.can_read() {
            Ok(Self(principal))
        } else {
            Err(CasError::InsufficientScope)
        }
    }
}

impl FromRequestParts<AppState> for WriteAccess {
    type Rejection = CasError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, CasError> {
        let principal = authenticate(parts, state)?;
        if principal.can_write() {
            Ok(Self(principal))
        } else {
            Err(CasError::InsufficientScope)
        }
    }
}

/// The principal of the request's `Authorization: Bearer <token>` header,
/// the only credential CAS routes accept (S10 "Auth"). The scheme is
/// case-insensitive (RFC 9110 §11.1); anything other than `Bearer`, or a
/// value that isn't visible ASCII, is an invalid credential.
fn authenticate(parts: &Parts, state: &AppState) -> Result<Principal, CasError> {
    let mut values = parts.headers.get_all(AUTHORIZATION).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(if parts.headers.contains_key(AUTHORIZATION) {
            AuthError::Invalid
        } else {
            AuthError::Missing
        }
        .into());
    };
    let token = value
        .to_str()
        .ok()
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim_start_matches(' '))
        .ok_or(AuthError::Invalid)?;
    Ok(state.auth.authenticate(token, state.clock.now())?)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::Method;
    use axum::http::{Request, StatusCode};
    use openxet_auth::{Mode, StaticToken};
    use tower::ServiceExt as _;

    use super::*;
    use crate::routes::{Routes, on};
    use crate::test_support;

    // The scheme is a constant so the repository's secret scanner, which
    // flags any literal bearer value, can tell these test tokens from leaks.
    const BEARER: &str = "Bearer";
    const READ: &str = "oxt_unit_read";
    const WRITE: &str = "oxt_unit_write";

    fn token(token: &str, mode: Mode) -> StaticToken {
        StaticToken {
            sha256: StaticToken::digest(token),
            scope: "s".to_owned(),
            mode,
            subject: "unit".to_owned(),
            tenant: None,
        }
    }

    /// A write-only route, which the CAS gains only in later tasks.
    async fn write_route(dir: &tempfile::TempDir) -> Router {
        let tokens = vec![token(READ, Mode::Read), token(WRITE, Mode::Write)];
        let routes = Routes::new().route(
            "/w",
            on(Method::POST, |WriteAccess(p): WriteAccess| async move {
                p.subject
            }),
        );
        test_support::serve(routes, test_support::state(dir.path(), tokens).await)
    }

    async fn call(router: &Router, auth: &[&str]) -> (StatusCode, String) {
        let mut request = Request::post("/w");
        for value in auth {
            request = request.header(AUTHORIZATION, *value);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn write_access_refuses_a_read_token_with_403() {
        let dir = tempfile::tempdir().unwrap();
        let router = write_route(&dir).await;

        let (status, body) = call(&router, &[&format!("{BEARER} {READ}")]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("\"insufficient_scope\""), "{body}");

        let (status, body) = call(&router, &[&format!("{BEARER} {WRITE}")]).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "unit"));
    }

    #[tokio::test]
    async fn more_than_one_authorization_header_is_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let router = write_route(&dir).await;

        let (status, body) = call(
            &router,
            &[&format!("{BEARER} {WRITE}"), &format!("{BEARER} {READ}")],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body.contains("\"invalid_credential\""), "{body}");
    }
}
