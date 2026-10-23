//! openxet's authentication: who a request acts for, and what it may do.
//!
//! Three credentials exist (design §5.4, §5.5; ADR 0007; rulings M1-R2,
//! M1-R3):
//!
//! - **Static tokens** for development, CI and small setups. Only their
//!   SHA-256 is held; see [`StaticToken`].
//! - **CAS tokens**: Ed25519-signed JWTs (`alg: EdDSA`), verified against a
//!   key set selected by the header's `kid`. A token is valid while
//!   `now < exp` and invalid from the `exp` second on, with no leeway: stock
//!   clients refresh by clock 30 s before `exp` and never react to a `401`.
//!   See [`TokenSigner`] and [`PublicKey`].
//! - **Signed transfer URLs**, each bound by HMAC-SHA256 to one xorb, one
//!   byte range and one expiry; see [`UrlSigner`].
//!
//! No error or `Debug` output in this crate ever contains a credential or a
//! key.

#![forbid(unsafe_code)]

mod error;
mod jwt;
mod signed_url;
mod static_tokens;

use serde::{Deserialize, Serialize};

pub use error::{AuthError, ForbiddenReason};
pub use jwt::{GeneratedKeypair, MAX_TOKEN_BYTES, PublicKey, TokenSigner, generate_keypair};
pub use signed_url::{TransferQuery, UrlSigner};
pub use static_tokens::StaticToken;

/// What a credential may do within its scope. `Write` implies `Read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Download and query.
    Read,
    /// Upload as well as everything `Read` allows.
    Write,
}

/// The authenticated caller of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// Who the credential acts for (a JWT's `sub`, a static token's subject).
    pub subject: String,
    /// The scope the credential is limited to (design §5.4).
    pub scope: String,
    /// What the credential may do within `scope`.
    pub mode: Mode,
}

impl Principal {
    /// Whether the principal may read. Every mode can.
    #[must_use]
    pub fn can_read(&self) -> bool {
        matches!(self.mode, Mode::Read | Mode::Write)
    }

    /// Whether the principal may write.
    #[must_use]
    pub fn can_write(&self) -> bool {
        self.mode == Mode::Write
    }
}

/// Verifies bearer credentials: static tokens and CAS tokens.
#[derive(Debug)]
pub struct Authenticator {
    static_tokens: Vec<StaticToken>,
    jwt_keys: Vec<PublicKey>,
}

impl Authenticator {
    /// An authenticator accepting `static_tokens` and CAS tokens signed by
    /// any of `jwt_keys`. Several keys allow rotation (ADR 0007 §3).
    #[must_use]
    pub fn new(static_tokens: Vec<StaticToken>, jwt_keys: Vec<PublicKey>) -> Self {
        Self {
            static_tokens,
            jwt_keys,
        }
    }

    /// Authenticates the token from an `Authorization: Bearer` header.
    ///
    /// `now` is Unix seconds. Static tokens are tried first, then CAS
    /// tokens.
    ///
    /// # Errors
    ///
    /// [`AuthError::Missing`] for an empty token, [`AuthError::Expired`] for
    /// a CAS token at or past its `exp`, and [`AuthError::Invalid`] for
    /// anything else that does not verify.
    pub fn authenticate(&self, bearer: &str, now: u64) -> Result<Principal, AuthError> {
        if bearer.is_empty() {
            return Err(AuthError::Missing);
        }
        if let Some(token) = static_tokens::find(&self.static_tokens, bearer) {
            return Ok(token.principal());
        }
        jwt::verify(&self.jwt_keys, bearer, now)
    }
}

/// The longest accepted scope, in bytes.
pub const MAX_SCOPE_BYTES: usize = 1024;

/// Checks a principal's subject and scope (ruling M1-R19): both non-empty
/// and free of control characters, so neither can widen a scope match or
/// forge lines in logs, and the scope at most [`MAX_SCOPE_BYTES`].
pub(crate) fn check_identity(subject: &str, scope: &str) -> Result<(), &'static str> {
    if subject.is_empty() {
        return Err("the subject is empty");
    }
    if subject.chars().any(char::is_control) {
        return Err("the subject contains control characters");
    }
    if scope.is_empty() {
        return Err("the scope is empty");
    }
    if scope.len() > MAX_SCOPE_BYTES {
        return Err("the scope is longer than 1024 bytes");
    }
    if scope.chars().any(char::is_control) {
        return Err("the scope contains control characters");
    }
    Ok(())
}
