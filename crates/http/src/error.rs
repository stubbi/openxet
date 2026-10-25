//! The JSON error body every route answers with, and the answers of the
//! composed router's own fallbacks.

use std::any::Any;
use std::borrow::Cow;

use axum::Json;
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// The schema of every error body.
pub const SCHEMA: &str = "openxet.error.v1";

/// `X-Error-Code`: huggingface.co's error code, which `huggingface_hub`
/// maps to its exception types (`RepoNotFound`, `RevisionNotFound`,
/// `EntryNotFound`).
pub const X_ERROR_CODE: HeaderName = HeaderName::from_static("x-error-code");

/// `X-Error-Message`: the message `huggingface_hub` shows when it reports
/// the error.
pub const X_ERROR_MESSAGE: HeaderName = HeaderName::from_static("x-error-message");

/// An error answer: `{"schema":"openxet.error.v1","code","message","hint"}`
/// with `Content-Type: application/json` and its status. `code` is stable
/// snake case, for agents; `hint` names the next action.
///
/// The body is sent as built and never logged here: the caller logs the
/// failure, with its cause, under the request's id.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBody {
    #[serde(skip)]
    status: StatusCode,
    schema: &'static str,
    code: &'static str,
    message: Cow<'static, str>,
    hint: &'static str,
}

impl ErrorBody {
    /// The answer `status` with this `code`, `message` and `hint`.
    #[must_use]
    pub fn new(
        status: StatusCode,
        code: &'static str,
        message: impl Into<Cow<'static, str>>,
        hint: &'static str,
    ) -> Self {
        Self {
            status,
            schema: SCHEMA,
            code,
            message: message.into(),
            hint,
        }
    }
}

impl IntoResponse for ErrorBody {
    fn into_response(self) -> Response {
        (self.status, Json(self)).into_response()
    }
}

/// A Hub-layer error answer (ruling M2-R6): the [`ErrorBody`] fields plus
/// huggingface.co's `error` field holding the message, and the
/// `X-Error-Code` and `X-Error-Message` headers `huggingface_hub` reads.
#[derive(Debug, Clone, Serialize)]
pub struct HubErrorBody {
    error: Cow<'static, str>,
    #[serde(flatten)]
    body: ErrorBody,
    #[serde(skip)]
    error_code: Option<&'static str>,
}

impl HubErrorBody {
    /// `body` as a Hub answer, with `X-Error-Code: {error_code}` when
    /// huggingface.co defines a code for the failure.
    #[must_use]
    pub fn new(body: ErrorBody, error_code: Option<&'static str>) -> Self {
        Self {
            error: body.message.clone(),
            body,
            error_code,
        }
    }
}

impl IntoResponse for HubErrorBody {
    fn into_response(self) -> Response {
        let message = header_text(&self.error);
        let error_code = self.error_code;
        let mut response = (self.body.status, Json(self)).into_response();
        let headers = response.headers_mut();
        if let Some(code) = error_code {
            headers.insert(X_ERROR_CODE, HeaderValue::from_static(code));
        }
        headers.insert(X_ERROR_MESSAGE, message);
        response
    }
}

/// `message` as a header value. A header carries only visible ASCII and
/// spaces, so any other character (a UTF-8 path, a control character)
/// becomes `?`; the body keeps the exact message.
fn header_text(message: &str) -> HeaderValue {
    let text: String = message
        .chars()
        .map(|c| {
            if c == ' ' || c.is_ascii_graphic() {
                c
            } else {
                '?'
            }
        })
        .collect();
    HeaderValue::try_from(text)
        .unwrap_or_else(|_| unreachable!("visible ASCII and spaces are a valid header value"))
}

const SERVER_FAULT_HINT: &str = "This is a server-side failure. Give the operator the \
     `X-Request-Id` response header: the server log records the cause under it.";

/// The fallback for paths no route serves: `404 not_found`, whatever the
/// method, which is how stock clients learn a route doesn't exist here
/// (S10 "Status codes and retries"). The text is M1's CAS answer, the same
/// as `openxet_cas::CasError::NoRoute`'s.
pub(crate) async fn no_route() -> ErrorBody {
    refused(ErrorBody::new(
        StatusCode::NOT_FOUND,
        "not_found",
        "this CAS server has no route for the request",
        "Check the endpoint: clients append `/v1/...` to the CAS URL, which has no path and \
         no trailing slash. The routes are listed at https://huggingface.co/docs/xet/api.",
    ))
}

/// The fallback for a served path with an unserved method:
/// `405 method_not_allowed`. axum adds the `Allow` header.
pub(crate) async fn method_not_allowed() -> ErrorBody {
    refused(ErrorBody::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "the route does not accept this HTTP method",
        "Use a method listed in the `Allow` response header.",
    ))
}

/// The response to a handler panic: `500 internal`, logged inside the
/// request's span. The panic payload is neither logged nor sent, since it
/// may hold request data.
pub(crate) fn panicked(_payload: Box<dyn Any + Send + 'static>) -> Response {
    tracing::error!(code = "internal", "a request handler panicked");
    ErrorBody::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "the server failed to handle the request",
        SERVER_FAULT_HINT,
    )
    .into_response()
}

/// `body`, logged as a refusal: by code only, the request's own fault.
fn refused(body: ErrorBody) -> ErrorBody {
    tracing::debug!(code = body.code, "request refused");
    body
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::to_bytes;

    use super::*;

    async fn json(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn error_body_is_the_m1_json() {
        let response =
            ErrorBody::new(StatusCode::CONFLICT, "busy", "it is busy", "Wait.").into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.headers()["content-type"], "application/json");
        assert_eq!(
            json(response).await,
            r#"{"schema":"openxet.error.v1","code":"busy","message":"it is busy","hint":"Wait."}"#
        );
    }

    #[tokio::test]
    async fn hub_error_body_adds_the_error_field_and_headers() {
        // Ruling M2-R6's unknown-repository answer, byte for byte.
        let body = ErrorBody::new(
            StatusCode::NOT_FOUND,
            "repo_not_found",
            "Repository not found",
            "Check the repository id and the token's access: a repository the token may not \
             read answers exactly like one that doesn't exist.",
        );
        let response = HubErrorBody::new(body, Some("RepoNotFound")).into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[X_ERROR_CODE], "RepoNotFound");
        assert_eq!(response.headers()[X_ERROR_MESSAGE], "Repository not found");
        assert_eq!(
            json(response).await,
            r#"{"error":"Repository not found","schema":"openxet.error.v1","code":"repo_not_found","message":"Repository not found","hint":"Check the repository id and the token's access: a repository the token may not read answers exactly like one that doesn't exist."}"#
        );
    }

    #[tokio::test]
    async fn hub_error_without_an_hf_code_sends_only_the_message_header() {
        let body = ErrorBody::new(
            StatusCode::UNAUTHORIZED,
            "missing_credential",
            "Authentication required: pass an openxet access token as the Bearer token (hf auth login)",
            "Log in.",
        );
        let response = HubErrorBody::new(body, None).into_response();
        assert!(!response.headers().contains_key(X_ERROR_CODE));
        assert_eq!(
            response.headers()[X_ERROR_MESSAGE],
            "Authentication required: pass an openxet access token as the Bearer token (hf auth login)"
        );
    }

    #[tokio::test]
    async fn the_message_header_replaces_what_a_header_cannot_carry() {
        let body = ErrorBody::new(
            StatusCode::NOT_FOUND,
            "entry_not_found",
            "no file café/a\nb",
            "Check the path.",
        );
        let response = HubErrorBody::new(body, Some("EntryNotFound")).into_response();
        assert_eq!(response.headers()[X_ERROR_MESSAGE], "no file caf?/a?b");
        assert!(
            json(response)
                .await
                .contains(r#""message":"no file café/a\nb""#)
        );
    }
}
