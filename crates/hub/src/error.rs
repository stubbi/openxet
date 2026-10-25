//! The Hub's errors and their answers (ruling M2-R6): huggingface.co's
//! `error` field and openxet's structured fields, with `X-Error-Code` where
//! huggingface.co defines one and `X-Error-Message` always, which
//! `huggingface_hub` reads to type and word its exceptions
//! (`utils/_http.py` lines 871–933, 1035–1078).

use std::error::Error as StdError;

use axum::body::Body;
use axum::http::header::{RETRY_AFTER, WWW_AUTHENTICATE};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::AuthzError;
use openxet_git::GitError;
use openxet_http::{ErrorBody, HubErrorBody};
use openxet_index::{IndexError, Oid};
use openxet_storage::StorageError;
use serde::Serialize;

/// The message of the unknown-repository answer, in the body's `error` and
/// `message` and in `X-Error-Message` (ruling M2-R6).
pub const REPO_NOT_FOUND: &str = "Repository not found";

/// The hint of the unknown-repository answer (ruling M2-R6).
const REPO_NOT_FOUND_HINT: &str = "Check the repository id and the token's access: a repository \
     the token may not read answers exactly like one that doesn't exist.";

/// `X-Repo-Commit`: the commit a read was answered at, which
/// `huggingface_hub` reads from an `EntryNotFound` answer to cache the
/// miss (`file_download.py` line 1773, S3).
pub const X_REPO_COMMIT: HeaderName = HeaderName::from_static("x-repo-commit");

/// What a missing credential answers on a Hub route (ruling M2-R6).
const MISSING_CREDENTIAL: &str =
    "Authentication required: pass an openxet access token as the Bearer token (hf auth login)";

/// What an invalid credential answers: the exact text `huggingface_hub`
/// reports as an authentication error rather than a missing repository
/// (`utils/_http.py` lines 933–935, S3 answer 6).
const INVALID_CREDENTIAL: &str = "Invalid credentials in Authorization header";

const NAME_HINT: &str = "Names are unique ignoring case and across labels (ADR 0010); choose \
     another name, or pass exist_ok=True to use the existing repository.";

const SERVER_FAULT_HINT: &str = "This is a server-side failure. Give the operator the \
     `X-Request-Id` response header: the server log records the cause under it.";

const RETRY_HINT: &str = "Retry in a moment. If it persists, give the operator the \
     `X-Request-Id` response header: the server log records the cause under it.";

/// A Hub request's failure. Its `Display` is the answer's message; nothing
/// in it is a credential.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HubError {
    /// The repository doesn't exist, or the caller may not read it. One
    /// constant answer for both, so it reveals nothing (ruling M2-R6).
    #[error("Repository not found")]
    UnknownRepo,
    /// No credential, and what was asked needs one. `401`.
    #[error(
        "Authentication required: pass an openxet access token as the Bearer token (hf auth login)"
    )]
    MissingCredential,
    /// The credential is unknown, malformed or expired. `401`.
    #[error("Invalid credentials in Authorization header")]
    InvalidCredential,
    /// A CAS credential with no user identity, which only CAS routes take
    /// (ruling M2-R3). `403`.
    #[error("this is a CAS token, which only the CAS routes accept; Hub routes need a user token")]
    CasCredential,
    /// The caller may read the repository, but it lacks the label of the
    /// typed route it was reached through (ADR 0010 item 4). `404` with
    /// `X-Error-Code: RepoNotFound`, so stock clients raise
    /// `RepositoryNotFoundError` with this message appended.
    #[error(
        "{repo} exists without the `{label}` label; add it with `openxet label add {repo} {label}` \
         (or the label API), or choose another name"
    )]
    WrongView {
        /// The repository, `{namespace}/{name}` as stored.
        repo: String,
        /// The label the route requires.
        label: &'static str,
    },
    /// The caller may not create in the namespace. `403` with S3's text
    /// (lesson 12), which `hf` shows from `X-Error-Message`.
    #[error("You don't have the rights to create a {noun} under the namespace \"{namespace}\"")]
    CreateForbidden {
        /// What the create asked for: `model`, `dataset`, …
        noun: &'static str,
        /// The namespace asked for.
        namespace: String,
    },
    /// The caller may read the repository but not do what was asked.
    /// `403`.
    #[error("You don't have the rights to {verb} {repo}")]
    Forbidden {
        /// What was refused, as a verb phrase: `write to`, `delete`.
        verb: &'static str,
        /// The repository, `{namespace}/{name}` as stored.
        repo: String,
    },
    /// A namespace or name breaks a rule of ruling M2-R2. `400`.
    #[error("{message}")]
    InvalidName {
        /// Which part breaks which rule.
        message: String,
        /// The rule, and how to comply.
        hint: &'static str,
    },
    /// The name is taken, ignoring case and across labels (ADR 0010 item
    /// 5). `409` with `url`, which `create_repo(exist_ok=True)` reads
    /// (S3 answer 5).
    #[error("{message}")]
    NameTaken {
        /// What exists. Labels only for a caller who may read it.
        message: String,
        /// The requested view's URL of the requested name.
        url: String,
    },
    /// The repository's labels changed since the request read them
    /// (ruling M2-R12): nothing was changed. `409`.
    #[error("the labels of {repo} changed concurrently")]
    LabelsChanged {
        /// The repository, `{namespace}/{name}` as stored, or its id.
        repo: String,
    },
    /// A label breaks a rule of ADR 0010 item 3. `400`.
    #[error("{message}")]
    InvalidLabel {
        /// Which rule the label breaks.
        message: String,
        /// The rule, and how to comply.
        hint: &'static str,
    },
    /// The request is malformed. `400`.
    #[error("{message}")]
    BadRequest {
        /// What is wrong.
        message: String,
        /// What a well-formed request looks like.
        hint: &'static str,
    },
    /// The request body is larger than the route takes. `413`.
    #[error("the request body is larger than the {limit} bytes this route takes")]
    BodyTooLarge {
        /// The route's limit, in bytes.
        limit: usize,
    },
    /// The request body arrived too slowly (`server.body_read_timeout`,
    /// `server.body_min_rate`). `408`.
    #[error("the request body arrived too slowly")]
    BodyTimeout,
    /// The revision is no branch or tag, nor a commit a branch or tag
    /// contains (ADR 0004 item 6). `404` with `X-Error-Code:
    /// RevisionNotFound` and huggingface.co's text (S3 fixture `25`).
    #[error("Invalid rev id: {revision}")]
    RevisionNotFound {
        /// The revision as the client named it.
        revision: String,
    },
    /// The path names no entry at the revision. `404` with `X-Error-Code:
    /// EntryNotFound`, huggingface.co's text (S3 fixture `18`) and
    /// `X-Repo-Commit`.
    #[error("{path} does not exist on \"{revision}\"")]
    EntryNotFound {
        /// The path as the client named it.
        path: String,
        /// The revision as the client named it.
        revision: String,
        /// The commit the revision resolved to.
        commit: Option<Oid>,
    },
    /// A commit's `parentCommit` isn't its branch's tip (ruling M2-R7).
    /// `412`.
    #[error("{0}")]
    ParentMismatch(String),
    /// A branch or tag to create exists already, or one to delete or move
    /// isn't where the request read it. `409` with `X-Error-Message`, which
    /// `create_branch(exist_ok=True)` swallows (S3 fixture `12`).
    #[error("{message}")]
    RefConflict {
        /// Which ref, and what about it.
        message: String,
    },
    /// A commit or ref write that can't be applied as asked: a bad path,
    /// branch or tag name, a delete of `main`, objects git refuses. `400`.
    #[error("{0}")]
    InvalidWrite(String),
    /// Too many reads or writes run in the repository at once, or a commit
    /// lost its branch to concurrent writers on every attempt. `503` with
    /// `Retry-After`.
    #[error("{0}")]
    Busy(String),
    /// A Git request of the dumb HTTP protocol, which isn't served: only
    /// smart HTTP is (ruling M2-R11). `404`.
    #[error(
        "this server serves Git over smart HTTP only: info/refs needs \
         ?service=git-upload-pack or ?service=git-receive-pack"
    )]
    DumbHttp,
    /// R22's reachability check is over a concurrency limit for the
    /// repository or the caller (ADR 0004 item 2). `429` with
    /// `Retry-After`.
    #[error("{0}")]
    Throttled(String),
    /// A Hub endpoint openxet doesn't implement (design §6). `501`, naming
    /// it, never a confusing `404`.
    #[error("{endpoint} is not implemented by this openxet server")]
    NotImplemented {
        /// The endpoint, as huggingface.co documents it.
        endpoint: &'static str,
    },
    /// The authorizer could not decide. `503`.
    #[error("the authorizer could not decide; retry in a moment")]
    AuthorizerUnavailable(#[source] AuthzError),
    /// The index or the bucket failed in a way a retry may fix. `503`.
    #[error("the server's storage is unavailable; retry in a moment")]
    Unavailable(#[source] Box<dyn StdError + Send + Sync>),
    /// A server bug or corrupt state. `500`.
    #[error("the server failed to handle the request")]
    Internal(#[source] Box<dyn StdError + Send + Sync>),
}

impl HubError {
    /// The status, stable code and hint of the answer.
    fn parts(&self) -> (StatusCode, &'static str, &'static str) {
        match self {
            Self::UnknownRepo => (StatusCode::NOT_FOUND, "repo_not_found", REPO_NOT_FOUND_HINT),
            Self::MissingCredential => (
                StatusCode::UNAUTHORIZED,
                "missing_credential",
                "Log in with `hf auth login --token <token>`, or send `Authorization: Bearer <token>`; the server's operator issues tokens.",
            ),
            Self::InvalidCredential => (
                StatusCode::UNAUTHORIZED,
                "invalid_credential",
                "The token is unknown, malformed or expired. Log in again with `hf auth login \
                 --token <token>` using a token the server's operator issued.",
            ),
            Self::CasCredential => (
                StatusCode::FORBIDDEN,
                "cas_credential",
                "Log in with a user token: a static token without a scope, or the dev token \
                 `openxet serve` prints in dev mode.",
            ),
            Self::WrongView { .. } => (
                StatusCode::NOT_FOUND,
                "wrong_label",
                "Labels never change the address: add the label, or use the route of a label \
                 the repository has (ADR 0010).",
            ),
            Self::CreateForbidden { .. } => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "Ask the server's operator for an [[auth.rules]] entry granting your token's \
                 subject `create` on `<namespace>/*`, or create the repository in a namespace you \
                 may create in.",
            ),
            Self::Forbidden { .. } => (
                StatusCode::FORBIDDEN,
                "forbidden",
                "Ask the server's operator for an [[auth.rules]] entry granting your token's \
                 subject that action on the repository.",
            ),
            Self::InvalidName { hint, .. } => (StatusCode::BAD_REQUEST, "invalid_name", hint),
            Self::NameTaken { .. } => (StatusCode::CONFLICT, "name_taken", NAME_HINT),
            Self::LabelsChanged { .. } => (
                StatusCode::CONFLICT,
                "labels_changed",
                "Labels changed concurrently; re-read and retry.",
            ),
            Self::InvalidLabel { hint, .. } => (StatusCode::BAD_REQUEST, "invalid_label", hint),
            Self::BadRequest { hint, .. } => (StatusCode::BAD_REQUEST, "bad_request", hint),
            Self::BodyTooLarge { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                "Send a smaller body.",
            ),
            Self::BodyTimeout => (
                StatusCode::REQUEST_TIMEOUT,
                "body_timeout",
                "Send the body without long pauses; the operator can lower \
                 `server.body_min_rate`.",
            ),
            Self::RevisionNotFound { .. } => (
                StatusCode::NOT_FOUND,
                "revision_not_found",
                "Name a branch, a tag, or a commit one of them contains; `list_repo_refs` lists \
                 the branches and tags.",
            ),
            Self::EntryNotFound { .. } => (
                StatusCode::NOT_FOUND,
                "entry_not_found",
                "Check the path and the revision; `list_repo_tree` lists the revision's paths.",
            ),
            Self::ParentMismatch(_) => (
                StatusCode::PRECONDITION_FAILED,
                "precondition_failed",
                "Another commit landed on the branch first. Read its current commit and commit \
                 again on top of it, or leave out parent_commit.",
            ),
            Self::RefConflict { .. } => (
                StatusCode::CONFLICT,
                "ref_conflict",
                "List the refs again (`list_repo_refs`): choose another name, or delete the \
                 existing ref first.",
            ),
            Self::InvalidWrite(_) => (
                StatusCode::BAD_REQUEST,
                "invalid_write",
                "Fix the request as the message says and send it again; nothing was changed.",
            ),
            Self::Busy(_) => (StatusCode::SERVICE_UNAVAILABLE, "busy", RETRY_HINT),
            Self::DumbHttp => (
                StatusCode::NOT_FOUND,
                "dumb_http_not_served",
                "Clone the repository's URL with git, which asks for smart HTTP itself; \
                 openxet serves no files under a repository's Git URL.",
            ),
            Self::Throttled(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_requests",
                RETRY_HINT,
            ),
            Self::NotImplemented { .. } => (
                StatusCode::NOT_IMPLEMENTED,
                "not_implemented",
                "openxet implements the Hub API huggingface_hub needs for repositories, uploads \
                 and downloads (design §6); this endpoint is not part of it.",
            ),
            Self::AuthorizerUnavailable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "authorizer_unavailable",
                RETRY_HINT,
            ),
            Self::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable", RETRY_HINT),
            Self::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                SERVER_FAULT_HINT,
            ),
        }
    }

    /// huggingface.co's `X-Error-Code`, where it defines one: a wrong view
    /// is `RepoNotFound` too, as huggingface.co answers a wrong
    /// `repo_type` (ADR 0010 item 4).
    fn error_code(&self) -> Option<&'static str> {
        match self {
            Self::UnknownRepo | Self::WrongView { .. } => Some("RepoNotFound"),
            Self::RevisionNotFound { .. } => Some("RevisionNotFound"),
            Self::EntryNotFound { .. } => Some("EntryNotFound"),
            _ => None,
        }
    }

    /// Logs the failure inside the request's span: a `5xx` at error with
    /// its cause, anything else at debug by code only.
    fn log(&self) {
        let (status, code, _) = self.parts();
        if status.is_server_error() {
            tracing::error!(code, error = self as &dyn StdError, "request failed");
        } else {
            tracing::debug!(code, "request refused");
        }
    }
}

impl IntoResponse for HubError {
    fn into_response(self) -> Response {
        self.log();
        let (status, code, hint) = self.parts();
        // The unknown-repository answer is built from constants only, so it
        // can't differ between the cases it covers (ruling M2-R6).
        let message = match &self {
            Self::UnknownRepo => REPO_NOT_FOUND.to_owned(),
            Self::MissingCredential => MISSING_CREDENTIAL.to_owned(),
            Self::InvalidCredential => INVALID_CREDENTIAL.to_owned(),
            other => other.to_string(),
        };
        let body = HubErrorBody::new(
            ErrorBody::new(status, code, message, hint),
            self.error_code(),
        );
        let mut response = match &self {
            Self::NameTaken { url, .. } => with_url(body, url),
            _ => body.into_response(),
        };
        let headers = response.headers_mut();
        match self {
            Self::MissingCredential => {
                headers.insert(
                    WWW_AUTHENTICATE,
                    HeaderValue::from_static(r#"Bearer realm="openxet""#),
                );
            }
            Self::InvalidCredential => {
                headers.insert(
                    WWW_AUTHENTICATE,
                    HeaderValue::from_static(r#"Bearer realm="openxet", error="invalid_token""#),
                );
            }
            Self::AuthorizerUnavailable(_)
            | Self::Unavailable(_)
            | Self::Busy(_)
            | Self::Throttled(_) => {
                headers.insert(RETRY_AFTER, HeaderValue::from_static("1"));
            }
            Self::EntryNotFound {
                commit: Some(commit),
                ..
            } => {
                if let Ok(value) = HeaderValue::try_from(commit.to_string()) {
                    headers.insert(X_REPO_COMMIT, value);
                }
            }
            _ => {}
        }
        response
    }
}

/// A Hub error body with huggingface.co's optional `url` field, which its
/// `409` carries (S3 fixtures `05`, `06`).
#[derive(Serialize)]
struct WithUrl<'a> {
    #[serde(flatten)]
    body: &'a HubErrorBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<&'a str>,
}

/// `body` with `url` added. Should serializing fail, which strings and
/// integers never make it, the answer keeps its status, headers and body
/// without `url`.
fn with_url(body: HubErrorBody, url: &str) -> Response {
    let bytes = serde_json::to_vec(&WithUrl {
        body: &body,
        url: Some(url),
    });
    let response = body.into_response();
    match bytes {
        Ok(bytes) => {
            let (parts, _) = response.into_parts();
            Response::from_parts(parts, Body::from(bytes))
        }
        Err(_) => response,
    }
}

impl From<IndexError> for HubError {
    fn from(e: IndexError) -> Self {
        match e {
            IndexError::Database(_) | IndexError::Migrate(_) | IndexError::OpenLock { .. } => {
                Self::Unavailable(Box::new(e))
            }
            // Deleted while the request ran: it no longer exists.
            IndexError::UnknownRepo { .. } => Self::UnknownRepo,
            IndexError::LabelsChanged { repo } => Self::LabelsChanged {
                repo: repo.to_string(),
            },
            _ => Self::Internal(Box::new(e)),
        }
    }
}

impl From<StorageError> for HubError {
    fn from(e: StorageError) -> Self {
        match e {
            StorageError::Io { .. } => Self::Unavailable(Box::new(e)),
            _ => Self::Internal(Box::new(e)),
        }
    }
}

impl HubError {
    /// The engine's failure `e` of a read or write at `revision`, which
    /// resolved to `commit`: an entry that isn't found names both.
    pub(crate) fn at(e: GitError, revision: &str, commit: Option<Oid>) -> Self {
        match e {
            GitError::EntryNotFound { path } => Self::EntryNotFound {
                path,
                revision: revision.to_owned(),
                commit,
            },
            e => e.into(),
        }
    }
}

impl From<GitError> for HubError {
    /// The engine's failures. An entry that isn't found is answered by
    /// the read that knows the revision; here it carries none.
    fn from(e: GitError) -> Self {
        match e {
            GitError::RevisionNotFound { revision } => Self::RevisionNotFound { revision },
            GitError::EntryNotFound { path } => Self::EntryNotFound {
                path,
                revision: String::new(),
                commit: None,
            },
            GitError::ParentMismatch { .. } => Self::ParentMismatch(e.to_string()),
            GitError::RefConflict { .. } => Self::RefConflict {
                message: e.to_string(),
            },
            GitError::InvalidHubWrite(message) => Self::InvalidWrite(message),
            GitError::MissingObjects => Self::InvalidWrite(e.to_string()),
            GitError::Busy | GitError::Contended { .. } | GitError::Refused(_) => {
                Self::Busy(e.to_string())
            }
            GitError::Index(e) => e.into(),
            GitError::Storage(e) => e.into(),
            _ => Self::Internal(Box::new(e)),
        }
    }
}

impl From<AuthzError> for HubError {
    fn from(e: AuthzError) -> Self {
        Self::AuthorizerUnavailable(e)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use axum::body::to_bytes;

    use super::*;

    async fn text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn name_taken_adds_url_to_the_hub_body() {
        let response = HubError::NameTaken {
            message: "Repository a/b already exists".to_owned(),
            url: "http://h/a/b".to_owned(),
        }
        .into_response();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            response.headers()["x-error-message"],
            "Repository a/b already exists"
        );
        assert_eq!(response.headers()["content-type"], "application/json");
        let body: serde_json::Value = serde_json::from_str(&text(response).await).unwrap();
        assert_eq!(body["error"], "Repository a/b already exists");
        assert_eq!(body["url"], "http://h/a/b");
        assert_eq!(body["code"], "name_taken");
        assert_eq!(body["schema"], "openxet.error.v1");
    }

    #[tokio::test]
    async fn storage_failures_are_retryable_503_and_faults_500() {
        let busy = HubError::from(StorageError::Io {
            op: "write",
            path: "/b".into(),
            source: std::io::Error::other("disk full"),
        })
        .into_response();
        assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(busy.headers()["retry-after"], "1");
        let corrupt = HubError::from(IndexError::Corrupt("zebra-row".to_owned())).into_response();
        assert_eq!(corrupt.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // The cause stays in the log; the body names none of it.
        assert!(!text(corrupt).await.contains("zebra-row"));
        let gone = HubError::from(IndexError::UnknownRepo {
            repo: openxet_index::RepoId([0; 16]),
        });
        assert!(matches!(gone, HubError::UnknownRepo));
    }
}
