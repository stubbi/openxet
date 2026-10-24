use std::error::Error as StdError;

use axum::Json;
use axum::http::header::{CONNECTION, CONTENT_RANGE, RETRY_AFTER, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use openxet_auth::{AuthError, ForbiddenReason};
use openxet_index::IndexError;
use openxet_protocol::{MerkleHash, ProtocolError, VerifyError, hash_to_hex};
use openxet_storage::StorageError;
use serde::Serialize;

use crate::BodyDeadline;

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

    /// A signed transfer URL was fetched without its signed query
    /// (`range`, `exp` and `sig`), or with one that doesn't parse.
    /// `400 invalid_transfer_url`.
    #[error("the transfer URL is not a signed URL from a reconstruction")]
    InvalidTransferUrl,

    /// No file with the path's hash is registered to the caller's scope.
    /// `404 not_found`. Another scope's file reads exactly as one never
    /// stored (design §5.4, ruling M1-R16), so the answer names nothing from
    /// the request.
    #[error("the file is not stored on this server for this credential's scope")]
    FileNotFound,

    /// A reconstruction's `Range` starts at or past the end of the file, or
    /// is the empty suffix `bytes=-0`. `416 range_not_satisfiable` with
    /// `Content-Range: bytes */{size}`, which `hf_xet` reads as the end of
    /// the file (S10 §1). A signed transfer range past the stored xorb is
    /// not this: the index and the bucket disagree, a `500`
    /// (`transfer::refused_read`).
    #[error("the requested range lies past the end of the content, which is {size} bytes")]
    RangeNotSatisfiable {
        /// The file's size.
        size: u64,
    },

    /// A reconstruction's `Range` header is not one range of the `bytes`
    /// unit. `400 invalid_range`.
    #[error(
        "the Range header is not a single byte range (bytes=start-end, bytes=start- or bytes=-length)"
    )]
    InvalidRange,

    /// A batch reconstruction names more files than it may.
    /// `400 batch_too_large`.
    #[error("the batch names {count} files; at most {limit} are answered per request")]
    BatchTooLarge {
        /// The distinct files named.
        count: usize,
        /// [`crate::MAX_BATCH_FILES`].
        limit: usize,
    },

    /// The files of a batch reconstruction hold more terms than one batch
    /// may plan. `400 batch_too_large`.
    #[error("the batch's files hold {terms} terms; at most {limit} are answered per request")]
    BatchTooManyTerms {
        /// The terms of the files answered.
        terms: usize,
        /// [`crate::MAX_BATCH_TERMS`].
        limit: usize,
    },

    /// A batch reconstruction body is not a JSON list of
    /// `{"prefix":"default","hash":…}` keys. `400 invalid_batch`. The body is
    /// never echoed.
    #[error("the batch body is not a JSON list of {{\"prefix\": \"default\", \"hash\": …}} keys")]
    InvalidBatch,

    /// A shard's file references a xorb that isn't visible to the caller:
    /// missing, or under `PerScope` another scope's, which reads the same
    /// (ruling M1-R17). `400 unknown_xorb`, which clients don't retry.
    ///
    /// The message carries the stale-cache note (ruling M1-R5): on
    /// `/v2/shards` it is the only text the user sees (S10 §4), and clients
    /// dedup against shards cached for up to 21 days, so a reset server is
    /// the usual cause.
    #[error(
        "xorb {} is not stored on this server. If the server was reset, clear the client's Xet \
         cache for this endpoint (~/.cache/huggingface/xet or $HF_XET_CACHE) and upload again.",
        hash_to_hex(.xorb)
    )]
    UnknownXorb {
        /// The first xorb found that isn't visible.
        xorb: MerkleHash,
    },

    /// A hash in the path is not 64 lowercase hex characters.
    /// `400 invalid_hash`. The segment is never echoed: it may be anything,
    /// a pasted credential included.
    #[error("the hash in the request path is not 64 lowercase hex characters")]
    InvalidHash,

    /// A path that names an object holds a percent-encoded byte, which would
    /// give the object a second URL. `400 invalid_path`. The path is never
    /// echoed.
    #[error("the request path is percent-encoded; object paths are sent unencoded")]
    InvalidPath,

    /// No xorb with the path's hash is visible to the caller.
    /// `404 xorb_not_found`; under `PerScope` visibility another scope's
    /// xorb reads the same as a missing one.
    #[error("xorb {} is not stored on this server", hash_to_hex(.xorb))]
    XorbNotFound {
        /// The xorb asked for.
        xorb: MerkleHash,
    },

    /// No xorb visible to the caller offers the queried chunk for global
    /// dedup. `404 not_found`, which `hf_xet` reads as "no match" and
    /// continues the upload (S10 §2). The answer is the same whether the
    /// chunk was never stored or, under `PerScope` visibility, is another
    /// scope's, and it names nothing from the request.
    #[error("no xorb on this server offers the chunk for deduplication")]
    NoDedupMatch,

    /// Every dedup permit stayed taken for the whole permit wait (ruling
    /// M1-R33). `429 dedup_busy` with `Retry-After: 1`; `hf_xet` reads a
    /// `429` from `/v1/chunks` as "no match" without retrying (S10 §2), so
    /// the upload continues without this dedup answer.
    #[error("the server is busy answering other dedup queries")]
    DedupBusy,

    /// The process's memory budget stayed full for the whole wait (ruling
    /// M1-R40). `503 memory_busy` with `Retry-After: 1`, which stock
    /// clients retry with backoff (S10 "Status codes and retries").
    #[error("the server is holding as much request data in memory as it may; try again shortly")]
    MemoryBusy,

    /// The request needs more memory than the process's whole budget
    /// (ruling M1-R40). `503 memory_busy` with `Retry-After: 1`, like
    /// [`CasError::MemoryBusy`]: stock clients can't tell the two apart,
    /// and the message says why a retry won't help.
    #[error(
        "the request needs about {needed} bytes of server memory, more than the server's whole \
         memory budget of {budget} bytes"
    )]
    OverMemoryBudget {
        /// The bytes the request needs.
        needed: u64,
        /// The budget, `server.memory_budget`.
        budget: u64,
    },

    /// An uploaded xorb's content hash is not the hash in its path.
    /// `400 xorb_hash_mismatch`, final.
    #[error(
        "the xorb body hashes to {}, but was uploaded as {}",
        hash_to_hex(.computed),
        hash_to_hex(.path)
    )]
    XorbHashMismatch {
        /// The hash in the request path.
        path: MerkleHash,
        /// The xorb hash computed from the body's chunks.
        computed: MerkleHash,
    },

    /// An uploaded xorb breaks a format rule or limit. `400` with `code`
    /// `xorb_<rule>`, final.
    #[error("{reason}")]
    InvalidXorb {
        /// `xorb_` and the snake-case name of the rule broken.
        code: &'static str,
        /// The rule broken, with the chunk it was found in.
        reason: Box<ProtocolError>,
    },

    /// An uploaded xorb body exceeds the limit. `413 xorb_too_large`.
    #[error("the xorb body is larger than this server accepts ({limit} bytes)")]
    XorbTooLarge {
        /// [`openxet_protocol::MAX_XORB_BODY_BYTES`].
        limit: u64,
    },

    /// An uploaded shard doesn't parse. `400 shard_malformed`, final.
    #[error("{0}")]
    MalformedShard(Box<ProtocolError>),

    /// An uploaded shard parses but fails verification against the stored
    /// xorbs, or exceeds the verification limits.
    /// `400 shard_verification_failed`, final. The message is the reason's
    /// client-safe `Display`; the values it withholds are logged
    /// under the request id (ruling M1-R23).
    #[error("shard verification failed: {0}")]
    ShardVerificationFailed(Box<VerifyError>),

    /// An uploaded shard body exceeds the limit. `413 shard_too_large`.
    #[error("the shard body is larger than this server accepts ({limit} bytes)")]
    ShardTooLarge {
        /// [`openxet_protocol::MAX_UPLOAD_SHARD_BYTES`].
        limit: u64,
    },

    /// The request body ended before its declared length, or its transfer
    /// was malformed. `400 bad_request`.
    #[error("the request body could not be read in full")]
    UnreadableBody,

    /// The request body stalled or fell below the server's minimum rate
    /// ([`crate::BodyDeadline`]). `408 body_timeout`, closing the
    /// connection.
    #[error(
        "the request body arrived too slowly: this server ends a body that sends nothing for \
         {:?} or averages under {} bytes per second",
        .0.idle(),
        .0.min_rate()
    )]
    BodyTimeout(BodyDeadline),

    /// The server is starting: its index and bucket are not open yet.
    /// `503 not_ready` with `Retry-After: 1`, which clients retry.
    #[error("the server is starting and not ready to serve requests yet")]
    NotReady,

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
            StorageError::Io { .. } => Self::Unavailable {
                component: "storage",
                source: e.into(),
            },
            // Every supported bucket is read-after-write consistent (ruling
            // M1-R21), so an object the index names but the bucket lacks is
            // data loss or a bug. A retry would only hide it.
            StorageError::NotFound { key } => {
                tracing::error!(
                    %key,
                    "integrity fault: the index names an object the bucket lacks"
                );
                Self::internal(e)
            }
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

/// The hint for a xorb a shard references but the server lacks; the
/// message itself carries the stale-cache note (ruling M1-R5).
const UNKNOWN_XORB_HINT: &str = "Upload every xorb before the shard that references it, with \
     `POST /v1/xorbs/default/{hash}` and a write token for this scope \
     (https://huggingface.co/docs/xet/upload-protocol#ordering-and-concurrency).";

const SHARD_HINT: &str = "Build the shard from the xorbs uploaded to this server, as stock \
     clients do (https://huggingface.co/docs/xet/shard#shard-upload). The message names the \
     rule broken and the file or xorb it was found in.";

const RETRY_HINT: &str = "Retry; stock clients do so automatically. If it persists, give the \
     operator the `X-Request-Id` response header: the server log records the cause under it.";

const CREDENTIAL_HINT: &str = "Send `Authorization: Bearer <token>` with a token issued for this \
     server; the token may have been revoked or signed by a key this server doesn't trust.";

impl CasError {
    /// The HTTP status, the stable code, and the hint.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per variant: the table of every code reads best whole"
    )]
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
            Self::InvalidTransferUrl => (
                StatusCode::BAD_REQUEST,
                "invalid_transfer_url",
                "Use a data URL exactly as a reconstruction (`GET /v2/reconstructions/{file}`) \
                 returned it, with its query.",
            ),
            Self::FileNotFound => (
                StatusCode::NOT_FOUND,
                "not_found",
                "Check the file hash, and use a token for the scope the file was uploaded to: \
                 files are visible only in their own scope.",
            ),
            Self::RangeNotSatisfiable { .. } => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
                "Nothing to fix when reading to the end: the range starts past the last byte. \
                 Ask for a range that starts below the size in `Content-Range`.",
            ),
            Self::InvalidRange => (
                StatusCode::BAD_REQUEST,
                "invalid_range",
                "Send one range, `Range: bytes={start}-{end}` with an inclusive end, or omit \
                 the header for the whole file (https://huggingface.co/docs/xet/api).",
            ),
            Self::BatchTooLarge { .. } => (
                StatusCode::BAD_REQUEST,
                "batch_too_large",
                "Split the files into batches of at most 1000.",
            ),
            Self::BatchTooManyTerms { .. } => (
                StatusCode::BAD_REQUEST,
                "batch_too_large",
                "Split the batch into fewer files per request, or reconstruct the largest files \
                 one at a time with `GET /v2/reconstructions/{file}`.",
            ),
            Self::InvalidBatch => (
                StatusCode::BAD_REQUEST,
                "invalid_batch",
                "Send a JSON list such as `[{\"prefix\": \"default\", \"hash\": \"<64 hex>\"}]`, \
                 or use `GET /v1/reconstructions?file_id=<hash>&file_id=<hash>` \
                 (https://huggingface.co/docs/xet/api#6-batch-file-reconstruction).",
            ),
            Self::UnknownXorb { .. } => {
                (StatusCode::BAD_REQUEST, "unknown_xorb", UNKNOWN_XORB_HINT)
            }
            Self::MalformedShard(_) => (StatusCode::BAD_REQUEST, "shard_malformed", SHARD_HINT),
            Self::ShardVerificationFailed(_) => (
                StatusCode::BAD_REQUEST,
                "shard_verification_failed",
                SHARD_HINT,
            ),
            Self::ShardTooLarge { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "shard_too_large",
                "Split the upload into shards of at most 64 MiB, as stock clients do \
                 (https://huggingface.co/docs/xet/upload-protocol#6-shard-serialization-and-upload).",
            ),
            Self::InvalidHash => (
                StatusCode::BAD_REQUEST,
                "invalid_hash",
                "Write the hash as 64 lowercase hex characters in Xet byte order \
                 (https://huggingface.co/docs/xet/api#converting-hashes-to-strings).",
            ),
            Self::InvalidPath => (
                StatusCode::BAD_REQUEST,
                "invalid_path",
                "Send the prefix and the 64-character lowercase hex hash as they are, without \
                 percent-encoding (https://huggingface.co/docs/xet/api#converting-hashes-to-strings).",
            ),
            Self::XorbNotFound { .. } => (
                StatusCode::NOT_FOUND,
                "xorb_not_found",
                "Upload the xorb with `POST /v1/xorbs/default/{hash}` and a write token for this \
                 scope.",
            ),
            Self::NoDedupMatch => (
                StatusCode::NOT_FOUND,
                "not_found",
                "Nothing to fix: upload the chunk's xorb as usual. Clients treat this as no match \
                 and continue the upload \
                 (https://huggingface.co/docs/xet/api#2-query-chunk-deduplication-global-deduplication).",
            ),
            Self::DedupBusy => (
                StatusCode::TOO_MANY_REQUESTS,
                "dedup_busy",
                "Nothing to fix: clients treat this as no match and continue the upload. If it \
                 persists, the server needs more CPUs for dedup answers.",
            ),
            Self::MemoryBusy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "memory_busy",
                "Retry after the `Retry-After` delay; stock clients do so automatically. If it \
                 persists, upload fewer files at once, or the operator can raise \
                 `server.memory_budget`.",
            ),
            Self::OverMemoryBudget { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "memory_busy",
                "Ask for less at once: split the batch, or reconstruct a range of the file. \
                 The operator can raise `server.memory_budget`.",
            ),
            Self::XorbHashMismatch { .. } => (
                StatusCode::BAD_REQUEST,
                "xorb_hash_mismatch",
                "Upload the xorb under its content hash: the xorb hash of its chunks \
                 (https://huggingface.co/docs/xet/hashing#xorb-hashes). Stock clients do; a \
                 mismatch means the body was changed or truncated on the way.",
            ),
            Self::InvalidXorb { code, .. } => (
                StatusCode::BAD_REQUEST,
                code,
                "Send the xorb as stock clients serialize it: its chunks, optionally followed by \
                 one footer (https://huggingface.co/docs/xet/xorb). The message names the rule \
                 broken and the chunk it was found in.",
            ),
            Self::XorbTooLarge { .. } => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "xorb_too_large",
                "Split the data into xorbs of at most 64 MiB of chunks, as stock clients do \
                 (https://huggingface.co/docs/xet/xorb#collecting-chunks).",
            ),
            Self::UnreadableBody => (
                StatusCode::BAD_REQUEST,
                "bad_request",
                "Resend the request with a body exactly as long as its `Content-Length`.",
            ),
            Self::BodyTimeout(_) => (
                StatusCode::REQUEST_TIMEOUT,
                "body_timeout",
                "Send the body without long pauses. On a slow or congested link, upload fewer \
                 files at once; the operator can lower `server.body_min_rate`.",
            ),
            Self::NotReady => (
                StatusCode::SERVICE_UNAVAILABLE,
                "not_ready",
                "Retry in a moment; stock clients do so automatically. `GET /readyz` answers \
                 `200` once the server is ready.",
            ),
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

impl CasError {
    /// Whether this is the server's fault rather than the request's: a
    /// `5xx`, whose cause is logged and never sent.
    pub(crate) fn is_server_error(&self) -> bool {
        self.parts().0.is_server_error()
    }

    /// Whether a retry may succeed: the index or the bucket was
    /// unavailable, or the memory budget full. A request's fault, one over
    /// the whole budget, or a server bug stays on retry.
    pub(crate) fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Unavailable { .. } | Self::NotReady | Self::MemoryBusy
        )
    }

    /// Logs the failure inside the request's span, so under its request id:
    /// - a `5xx` at error, with its cause and the cause's sources
    /// - a rejected shard at warn, with its full reason: a parse failure
    ///   with its sources, a verification failure with the values derived
    ///   from stored chunks that its message withholds (ruling M1-R23). The
    ///   v1 client never shows the body, so this is where the reason is read.
    /// - anything else at debug, by code only
    pub(crate) fn log(&self) {
        let (status, code, _) = self.parts();
        match self {
            Self::MalformedShard(reason) => {
                tracing::warn!(code, error = &**reason as &dyn StdError, "shard rejected");
            }
            Self::ShardVerificationFailed(reason) => {
                tracing::warn!(code, reason = %reason.detail(), "shard rejected");
            }
            Self::UnknownXorb { .. } => tracing::warn!(code, reason = %self, "shard rejected"),
            // The server's state, not a fault: one line per refusal, without
            // an error's alarm.
            Self::MemoryBusy | Self::OverMemoryBudget { .. } => {
                tracing::warn!(code, reason = %self, "request refused");
            }
            _ if status.is_server_error() => {
                tracing::error!(code, error = self as &dyn StdError, "request failed");
            }
            _ => tracing::debug!(code, "request refused"),
        }
    }
}

impl IntoResponse for CasError {
    fn into_response(self) -> Response {
        let (status, code, hint) = self.parts();
        self.log();
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
        if let Self::NotReady | Self::DedupBusy | Self::MemoryBusy | Self::OverMemoryBudget { .. } =
            self
        {
            response
                .headers_mut()
                .insert(RETRY_AFTER, HeaderValue::from_static("1"));
        }
        if let Self::RangeNotSatisfiable { size } = self {
            // RFC 9110 §15.5.17: a `416` names the current length.
            response.headers_mut().insert(
                CONTENT_RANGE,
                HeaderValue::from_str(&format!("bytes */{size}"))
                    .unwrap_or_else(|_| unreachable!("digits are a valid header value")),
            );
        }
        response
    }
}

/// The response to `e` for a request whose body is left unread. It closes
/// the connection: the unread bytes would otherwise be parsed as the next
/// request on a pooled connection.
pub(crate) fn closing(e: CasError) -> Response {
    let mut response = e.into_response();
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
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
    async fn a_slow_body_is_408_naming_the_bounds() {
        let deadline = BodyDeadline::new(Duration::from_secs(7), 512).unwrap();
        let reply = assert_maps(
            CasError::BodyTimeout(deadline),
            StatusCode::REQUEST_TIMEOUT,
            "body_timeout",
        )
        .await;
        let message = reply.body["message"].as_str().unwrap();
        assert!(
            message.contains("7s") && message.contains("512"),
            "{message}"
        );
    }

    #[tokio::test]
    async fn a_full_memory_budget_is_a_retryable_503_with_retry_after() {
        for (e, retryable) in [
            (CasError::MemoryBusy, true),
            (
                CasError::OverMemoryBudget {
                    needed: 2048,
                    budget: 1024,
                },
                false,
            ),
        ] {
            assert_eq!(e.is_retryable(), retryable);
            let reply = assert_maps(e, StatusCode::SERVICE_UNAVAILABLE, "memory_busy").await;
            assert_eq!(reply.headers[RETRY_AFTER], "1");
        }
    }

    #[tokio::test]
    async fn not_ready_is_a_retryable_503_with_retry_after() {
        let reply = assert_maps(
            CasError::NotReady,
            StatusCode::SERVICE_UNAVAILABLE,
            "not_ready",
        )
        .await;
        assert_eq!(reply.headers[RETRY_AFTER], "1");
        assert!(CasError::NotReady.is_retryable());
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
    async fn unknown_xorb_is_final_400_with_the_stale_cache_note() {
        // The note is in the message: on `/v2/shards` the message is the
        // only text the user sees (S10 §4).
        let xorb = MerkleHash::default();
        let reply = assert_maps(
            IndexError::UnknownXorb { xorb },
            StatusCode::BAD_REQUEST,
            "unknown_xorb",
        )
        .await;
        let message = reply.body["message"].as_str().unwrap();
        assert!(message.contains(&hash_to_hex(&xorb)), "{message}");
        assert!(
            message.contains("~/.cache/huggingface/xet or $HF_XET_CACHE"),
            "{message}"
        );
        assert!(
            reply.body["hint"]
                .as_str()
                .unwrap()
                .contains("POST /v1/xorbs/default/{hash}")
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

    #[test]
    fn storage_not_found_is_500_internal_and_logged_as_an_integrity_fault() {
        // Object stores are read-after-write consistent (ruling M1-R21): an
        // object the index names but the bucket lacks is data loss or a bug.
        let key = Key::Xorb(MerkleHash::default());
        let capture = Capture::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();

        let error = tracing::subscriber::with_default(subscriber, || {
            CasError::from(StorageError::NotFound { key })
        });

        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(logs.contains("ERROR"), "{logs}");
        assert!(logs.contains("integrity fault"), "{logs}");
        assert!(logs.contains(&key.to_string()), "{logs}");
        let reply = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(assert_maps(
                error,
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
            ));
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
