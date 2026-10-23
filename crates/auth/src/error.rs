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

    /// A signed-URL lifetime below [`crate::UrlSigner::MIN_LIFETIME`] was
    /// configured.
    #[error(
        "the signed-URL lifetime of {lifetime_secs} s is below the 1800 s (30 min) floor; configure at least 30 minutes"
    )]
    LifetimeTooShort {
        /// The rejected lifetime, in seconds.
        lifetime_secs: u64,
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

    /// The signature does not match the xorb, range and expiry.
    #[error("the signature does not match the xorb, range and expiry")]
    BadSignature,
}
