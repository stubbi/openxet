//! Transfer failures, classified from the client core's errors.
//!
//! The client core's own messages are never passed on, with one exception:
//! a shard the server refused. Its message is the server's, written for the
//! user (S10 §4, ruling M1-R5), and the only place a reason reaches them.

use std::io;
use std::path::PathBuf;

use xet_client::ClientError as CoreClientError;
use xet_client::cas_client::auth::AuthError;
use xet_data::DataError;
use xet_data::file_reconstruction::FileReconstructionError;

/// Why an upload or download failed.
///
/// Invariant: no message holds a credential, a signed URL, or server text
/// other than an upload rejection's message.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The endpoint is not a URL the client can use.
    #[error("the endpoint is not usable: {reason}")]
    InvalidEndpoint { reason: &'static str },
    /// The token expires within [`crate::EXPIRY_MARGIN_SECS`], or expired
    /// mid-transfer.
    #[error("the token expired (exp {exp})")]
    TokenExpired { exp: u64 },
    /// Nothing answered at the endpoint.
    #[error("could not connect to {endpoint}: {reason}")]
    Unreachable { endpoint: String, reason: String },
    /// `401`: the server doesn't accept the token.
    #[error("{endpoint} refused the token (401 Unauthorized)")]
    Unauthorized { endpoint: String },
    /// `403`: the token doesn't allow this, e.g. a read token uploading.
    #[error("{endpoint} does not allow this with the token (403 Forbidden)")]
    Forbidden { endpoint: String },
    /// No file with this hash in the token's scope.
    #[error("no file with hash {hash} in this token's scope")]
    NotFound { hash: String },
    /// The server refused a shard; `message` is the server's.
    #[error("the server rejected the upload: {message}")]
    UploadRejected { message: String },
    /// Some upload paths are not readable regular files. Nothing was sent.
    #[error("{} path(s) can't be uploaded: {}", problems.len(), list(problems))]
    InvalidInput { problems: Vec<InputProblem> },
    /// A hash is not 64 lowercase hex characters.
    #[error("the hash is not 64 lowercase hex characters")]
    InvalidHash,
    /// A local file couldn't be read or written.
    #[error("{}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
    /// The server answered with an error status the client gave up on.
    #[error("the server answered {status} and the client gave up after retrying")]
    Status { status: u16 },
    /// Any other failure of the client core, named by its kind only.
    #[error("the transfer failed ({kind})")]
    Transfer { kind: &'static str },
}

/// A path [`crate::upload`] can't read.
#[derive(Debug)]
pub struct InputProblem {
    pub path: PathBuf,
    pub problem: Problem,
}

/// Why a path can't be uploaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Problem {
    NotFound,
    /// A directory or another non-regular file.
    NotAFile,
    Unreadable(io::ErrorKind),
}

impl Problem {
    /// A stable name: `not_found`, `not_a_file` or `unreadable`.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::NotAFile => "not_a_file",
            Self::Unreadable(_) => "unreadable",
        }
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("no such file"),
            Self::NotAFile => f.write_str("not a regular file"),
            Self::Unreadable(kind) => write!(f, "can't be read ({kind})"),
        }
    }
}

fn list(problems: &[InputProblem]) -> String {
    problems
        .iter()
        .map(|p| format!("{} ({})", p.path.display(), p.problem))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the failed operation was about, to name it in the error.
pub(crate) struct Context<'a> {
    pub endpoint: &'a str,
    /// The file hash, for a download.
    pub hash: Option<&'a str>,
    /// The local file a bare I/O error is about.
    pub path: Option<&'a std::path::Path>,
    pub token_exp: u64,
}

impl ClientError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Classifies a client-core failure.
    pub(crate) fn from_data(err: DataError, cx: &Context<'_>) -> Self {
        match err {
            DataError::ClientError(err) => Self::from_core(&err, cx),
            DataError::FileReconstructionError(err) => Self::from_reconstruction(err, cx),
            DataError::AuthError(_) => Self::TokenExpired { exp: cx.token_exp },
            DataError::IOError(err) => Self::local_io(err, cx),
            DataError::HashStringParsingFailure(_) => Self::InvalidHash,
            other => Self::Transfer {
                kind: data_kind(&other),
            },
        }
    }

    fn from_reconstruction(err: FileReconstructionError, cx: &Context<'_>) -> Self {
        match err {
            FileReconstructionError::ClientError(err) => Self::from_core(&err, cx),
            FileReconstructionError::IoError(err) => {
                Self::local_io(io::Error::new(err.kind(), err.to_string()), cx)
            }
            FileReconstructionError::CorruptedReconstruction(_) => Self::Transfer {
                kind: "the server's reconstruction did not match the file",
            },
            _ => Self::Transfer {
                kind: "reconstruction",
            },
        }
    }

    fn from_core(err: &CoreClientError, cx: &Context<'_>) -> Self {
        let endpoint = cx.endpoint.to_owned();
        if let Some(status) = err.status() {
            return Self::from_status(status.as_u16(), cx);
        }
        match err {
            CoreClientError::FileNotFound(_) => Self::NotFound {
                hash: cx.hash.unwrap_or_default().to_owned(),
            },
            // The v2 shard route's error event (`shard_upload_v2.rs`): the
            // server's message for the user.
            CoreClientError::Other(message) => Self::UploadRejected {
                message: message.clone(),
            },
            CoreClientError::AuthError(
                AuthError::RefreshFunctionNotCallable(_) | AuthError::TokenRefreshFailure(_),
            ) => Self::TokenExpired { exp: cx.token_exp },
            CoreClientError::AuthError(_) => Self::Transfer { kind: "auth" },
            CoreClientError::IOError(err) => {
                Self::local_io(io::Error::new(err.kind(), err.to_string()), cx)
            }
            _ => match connect_failure(err) {
                Some(err) => Self::Unreachable {
                    endpoint,
                    reason: if err.is_timeout() {
                        "the connection timed out".to_owned()
                    } else {
                        "the connection failed".to_owned()
                    },
                },
                None => Self::Transfer {
                    kind: core_kind(err),
                },
            },
        }
    }

    fn from_status(status: u16, cx: &Context<'_>) -> Self {
        let endpoint = cx.endpoint.to_owned();
        match status {
            401 => Self::Unauthorized { endpoint },
            403 => Self::Forbidden { endpoint },
            404 if cx.hash.is_some() => Self::NotFound {
                hash: cx.hash.unwrap_or_default().to_owned(),
            },
            status => Self::Status { status },
        }
    }

    fn local_io(err: io::Error, cx: &Context<'_>) -> Self {
        match cx.path {
            Some(path) => Self::io(path, err),
            None => Self::Transfer { kind: "local I/O" },
        }
    }
}

/// The HTTP error, if the request never reached a server.
fn connect_failure(err: &CoreClientError) -> Option<&reqwest::Error> {
    let (CoreClientError::ReqwestError(err, _)
    | CoreClientError::ReqwestMiddlewareError(reqwest_middleware::Error::Reqwest(err))) = err
    else {
        return None;
    };
    (err.is_connect() || err.is_timeout()).then_some(err)
}

fn data_kind(err: &DataError) -> &'static str {
    match err {
        DataError::CASConfigError(_)
        | DataError::ShardConfigError(_)
        | DataError::DedupConfigError(_)
        | DataError::ParameterError(_) => "client configuration",
        DataError::FormatError(_) => "format",
        DataError::RuntimeError(_) | DataError::JoinError(_) => "runtime",
        _ => "internal",
    }
}

fn core_kind(err: &CoreClientError) -> &'static str {
    match err {
        CoreClientError::InvalidResponse(_) | CoreClientError::FormatError(_) => {
            "the server's answer could not be read"
        }
        CoreClientError::XORBNotFound(_) => "a xorb the server named is missing",
        CoreClientError::PresignedUrlExpirationError => "a transfer URL expired",
        CoreClientError::CacheError(_) | CoreClientError::ShardDedupDBError(_) => "cache",
        CoreClientError::ReqwestError(..) | CoreClientError::ReqwestMiddlewareError(_) => "HTTP",
        _ => "internal",
    }
}
