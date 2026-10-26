use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use openxet_auth::{AuthError, Credential, Principal};

use crate::{AppState, CasError};

/// A caller allowed to read: any valid CAS credential. A user-only
/// credential answers `403 insufficient_scope` (ruling M2-R3).
///
/// The extractor only authenticates. Handlers check the principal's scope
/// against the data they serve (design §5.4).
#[derive(Debug, Clone)]
pub struct ReadAccess(pub Principal);

/// A caller allowed to write: a valid CAS credential in write mode. A read
/// or user-only credential answers `403 insufficient_scope`.
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

/// The CAS principal of the request's `Authorization: Bearer <token>`
/// header, the only credential CAS routes accept (S10 "Auth"). The scheme
/// is case-insensitive (RFC 9110 §11.1); anything other than `Bearer`, or a
/// value that isn't visible ASCII, is an invalid credential. A user-only
/// credential holds no scope, so it may use no CAS route (ruling M2-R3).
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
    match state.auth.authenticate(token, state.clock.now())? {
        Credential::Cas(principal) | Credential::Both(principal, _) => Ok(principal),
        Credential::User(_) => Err(CasError::UserCredential),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::Method;
    use axum::http::{Request, StatusCode};
    use openxet_auth::{CasGrant, Mode, StaticToken, UserProfile};
    use tower::ServiceExt as _;

    use super::*;
    use crate::routes::{Routes, doc, on};
    use crate::test_support;

    // The scheme is a constant so the repository's secret scanner, which
    // flags any literal bearer value, can tell these test tokens from leaks.
    const BEARER: &str = "Bearer";
    const READ: &str = "oxt_unit_read";
    const WRITE: &str = "oxt_unit_write";
    const USER: &str = "oxt_unit_user";

    fn token(token: &str, mode: Mode) -> StaticToken {
        StaticToken {
            sha256: StaticToken::digest(token),
            subject: "unit".to_owned(),
            cas: Some(CasGrant {
                scope: "s".to_owned(),
                mode,
                tenant: None,
            }),
            user: None,
        }
    }

    /// A write-only route, which the CAS gains only in later tasks.
    async fn write_route(dir: &tempfile::TempDir) -> Router {
        let user = StaticToken {
            sha256: StaticToken::digest(USER),
            subject: "alice".to_owned(),
            cas: None,
            user: Some(UserProfile::default()),
        };
        let tokens = vec![token(READ, Mode::Read), token(WRITE, Mode::Write), user];
        let routes = Routes::new().route(
            "/w",
            on(
                Method::POST,
                doc("Write", true),
                |WriteAccess(p): WriteAccess| async move { p.subject },
            ),
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

    /// A user credential has no CAS scope: CAS routes refuse it, and say
    /// where a CAS token comes from (ruling M2-R3).
    #[tokio::test]
    async fn a_user_only_credential_is_refused_with_403() {
        let dir = tempfile::tempdir().unwrap();
        let router = write_route(&dir).await;

        let (status, body) = call(&router, &[&format!("{BEARER} {USER}")]).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("\"insufficient_scope\""), "{body}");
        assert!(
            body.contains("This is a user token; get a CAS token from the Hub token route"),
            "{body}"
        );
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
