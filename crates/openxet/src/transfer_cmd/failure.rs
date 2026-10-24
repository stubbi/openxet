//! Stable codes and hints for client failures.

use openxet_client::ClientError;

use crate::error::{CliError, ExitCode, FileError};

pub(super) fn usage_error(code: &'static str, message: &str, hint: &'static str) -> CliError {
    CliError::new(code, message, hint).with_exit(ExitCode::USAGE)
}

/// The stable code, message and hint for a client failure. The message is
/// the client's, which never holds the token (`ClientError`'s invariant).
pub(super) fn client_error(err: &ClientError) -> CliError {
    let message = err.to_string();
    let (code, hint): (&'static str, &'static str) = match err {
        ClientError::InvalidEndpoint { .. } => {
            return usage_error(
                "invalid_endpoint",
                &message,
                "Set OPENXET_ENDPOINT (or --endpoint) to the endpoint `openxet serve` prints, \
                 such as http://127.0.0.1:8080.",
            );
        }
        ClientError::InvalidHash => {
            return usage_error(
                "invalid_hash",
                &message,
                "Pass the Xet hash `openxet upload` printed for the file: 64 characters 0-9 \
                 and a-f.",
            );
        }
        ClientError::OutputExists { .. } => {
            return usage_error(
                "output_exists",
                &message,
                "Pass `--force` to replace it, or choose another `--output` path.",
            );
        }
        ClientError::InvalidInput { problems } => {
            let files = problems
                .iter()
                .map(|p| FileError {
                    path: p.path.to_string_lossy().into_owned(),
                    problem: p.problem.code(),
                })
                .collect();
            return usage_error(
                "invalid_input",
                &format!("{message}; nothing was uploaded"),
                "Pass paths of existing, readable regular files; directories are not uploaded.",
            )
            .with_files(files);
        }
        ClientError::TokenExpired { .. } => (
            "token_expired",
            "Get a new token, for example with `openxet token mint`, and set OPENXET_TOKEN.",
        ),
        ClientError::Unreachable { proxy: Some(_), .. } => (
            "unreachable_endpoint",
            "Requests go through the proxy named, which didn't answer: check that it is \
             running, or list the endpoint's host in NO_PROXY to bypass it.",
        ),
        ClientError::Unreachable { proxy: None, .. } => (
            "unreachable_endpoint",
            "Check that the server is running and that OPENXET_ENDPOINT (or --endpoint) is the \
             endpoint it printed on startup.",
        ),
        ClientError::Unauthorized { .. } => (
            "unauthorized",
            "Check that OPENXET_TOKEN is a token for this server, not revoked or expired. A dev \
             server prints its token on startup and keeps it in <data dir>/dev-token.",
        ),
        ClientError::Forbidden { .. } => (
            "forbidden",
            "The token is valid but doesn't allow this: uploading needs a write token for the \
             scope. Ask the operator for one, or mint one with `openxet token mint --mode \
             write`.",
        ),
        ClientError::NotFound { .. } => (
            "not_found",
            "Check the hash `openxet upload` printed, and that the token's scope holds the file.",
        ),
        ClientError::SizeMismatch { .. } => (
            "size_mismatch",
            "The size or the hash is wrong: pass the size `openxet upload` printed for this \
             hash, or omit `--size`. Nothing was written.",
        ),
        ClientError::UploadRejected { .. } => (
            "upload_rejected",
            "The message is the server's reason; follow it. Nothing from this upload was \
             registered.",
        ),
        ClientError::Io { .. } => (
            "io",
            "Check that the path exists and that you may read it (upload) or write its \
             directory (download).",
        ),
        ClientError::Status { .. } => (
            "server_error",
            "Retry later. If it keeps failing, give the server's operator the time of this run: \
             the server log records the cause.",
        ),
        ClientError::Transfer { .. } => (
            "transfer_failed",
            "Retry. If it keeps failing, report it with the command you ran.",
        ),
    };
    CliError::new(code, message, hint)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use openxet_client::Endpoint;

    use super::*;

    #[test]
    fn client_errors_never_carry_the_token_and_always_a_hint() {
        let token = "oxt_secret-marker";
        let endpoint = || "http://127.0.0.1:1".to_owned();
        let errors = [
            Endpoint::new("http://u:oxt_secret-marker@h", token.into()).unwrap_err(),
            ClientError::TokenExpired { exp: 1 },
            ClientError::Unreachable {
                endpoint: endpoint(),
                proxy: None,
                reason: "the connection was refused".into(),
            },
            ClientError::Unreachable {
                endpoint: endpoint(),
                proxy: Some("http://proxy:3128".into()),
                reason: "the connection was refused".into(),
            },
            ClientError::Unauthorized {
                endpoint: endpoint(),
            },
            ClientError::Forbidden {
                endpoint: endpoint(),
            },
            ClientError::NotFound {
                hash: "ab".repeat(32),
            },
            ClientError::SizeMismatch {
                expected: 1,
                actual: 2,
            },
            ClientError::OutputExists { path: "o".into() },
            ClientError::UploadRejected {
                message: "xorb … is not stored on this server".into(),
            },
            ClientError::InvalidHash,
            ClientError::Status { status: 503 },
            ClientError::Transfer { kind: "internal" },
        ];
        for err in errors {
            let cli = client_error(&err);
            assert!(!cli.message.contains("secret-marker"), "{cli:?}");
            assert!(!cli.hint.is_empty(), "{cli:?}");
        }
    }
}
