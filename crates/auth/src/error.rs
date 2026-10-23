use std::time::Duration;

/// An authentication failure.
///
/// No variant carries the presented credential, so neither `Debug` nor
/// `Display` can leak it into logs or responses (design §5.9).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum AuthError {
    /// No credential was presented. HTTP `401`.
    #[error(
        "no credential was presented; send `Authorization: Bearer <token>` with a static token or a CAS token"
    )]
    Missing,

    /// The credential is unknown, malformed, signed by an untrusted key or
    /// with an algorithm other than `EdDSA`. HTTP `401`.
    #[error(
        "the credential is not valid: it is unknown, malformed or not signed by a trusted key; obtain a new token"
    )]
    Invalid,

    /// The CAS token is at or past its `exp`. HTTP `401`.
    #[error(
        "the CAS token has expired (a token is invalid from its `exp` second on); obtain a new token"
    )]
    Expired,

    /// A signed transfer URL was refused. HTTP `403`.
    #[error("the signed transfer URL was refused: {reason}; request a fresh reconstruction")]
    Forbidden {
        /// Why the URL was refused.
        reason: ForbiddenReason,
    },

    /// A signed transfer URL was fetched without a `Range` header. HTTP `400`.
    #[error(
        "a signed transfer URL must be fetched with `Range: bytes={{start}}-{{end}}` equal to its `range` query parameter"
    )]
    BadRange,

    /// A configured key could not be loaded.
    #[error("the Ed25519 key could not be loaded: {reason}")]
    InvalidKey {
        /// What is wrong with the key; never the key itself.
        reason: &'static str,
    },

    /// A CAS token was requested that the verifier would reject.
    #[error("refusing to sign the CAS token: {reason}")]
    InvalidClaims {
        /// What is wrong with the requested claims.
        reason: &'static str,
    },

    /// A configured static token has an invalid subject or scope.
    #[error(
        "static token #{index} is misconfigured: {reason}; give every static token a non-empty subject and scope without control characters (scope at most 1024 bytes)"
    )]
    InvalidStaticToken {
        /// The token's position in the configured list, counting from 0.
        index: usize,
        /// What is wrong; never the token or its hash.
        reason: &'static str,
    },

    /// A configured lifetime is outside its accepted range: the signed-URL
    /// lifetime ([`crate::UrlSigner::MIN_LIFETIME`] to
    /// [`crate::UrlSigner::MAX_LIFETIME`]) or the CAS-token lifetime cap
    /// ([`crate::Authenticator::MIN_TOKEN_LIFETIME`] to
    /// [`crate::Authenticator::MAX_TOKEN_LIFETIME`]). Durations print
    /// exactly, so a value just past a bound never reads as the bound.
    #[error(
        "the {setting} {lifetime:?} is outside {min:?} to {max:?} inclusive; configure a value in that range"
    )]
    LifetimeOutOfRange {
        /// Which setting was rejected.
        setting: &'static str,
        /// The rejected value.
        lifetime: Duration,
        /// The smallest accepted value.
        min: Duration,
        /// The largest accepted value.
        max: Duration,
    },

    /// A transfer URL was requested for a range whose start is after its
    /// end, which no `Range` header could ever match.
    #[error("cannot sign byte range {start}-{end}: the start is after the end")]
    ReversedRange {
        /// The requested first byte.
        start: u64,
        /// The requested last byte, inclusive.
        end: u64,
    },

    /// The operating system's random number generator failed.
    #[error(
        "the operating system's random number generator failed while generating a key; retry, and check the host's entropy source if it persists"
    )]
    KeyGeneration,
}

/// Why a signed transfer URL was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ForbiddenReason {
    /// The `Range` header is not exactly `bytes={start}-{end}` for the URL's
    /// signed range.
    #[error("the Range header does not equal the range the URL was signed for")]
    RangeMismatch,

    /// The URL is at or past its `exp`.
    #[error("the URL has expired")]
    Expired,

    /// The URL's `exp` lies further ahead than this node's lifetime plus
    /// clock skew allows, so it was not issued under this configuration.
    #[error("the URL expires later than this server's URL lifetime allows")]
    ExpiryBeyondLifetime,

    /// The signature does not match the xorb, range and expiry.
    #[error("the signature does not match the xorb, range and expiry")]
    BadSignature,
}
