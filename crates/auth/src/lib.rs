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
//! - **Capability `href`s**, each bound by HMAC-SHA256 to one scope, one
//!   operation and one expiry (ADR 0003 decision 2); see
//!   [`CapabilitySigner`].
//!
//! A bearer credential authenticates as a [`Credential`]: a CAS credential
//! ([`Principal`]: scope, tenant, mode), a user credential
//! ([`UserPrincipal`]: subject and claims), or both (ruling M2-R3, ADR
//! 0012). CAS routes take the first; the Hub, Git and LFS routes ask an
//! [`Authorizer`] what the second may do per repository.
//!
//! No error or `Debug` output in this crate ever contains a credential or a
//! key.

#![forbid(unsafe_code)]

mod capability;
mod error;
mod jwt;
mod rules;
mod signed_url;
mod static_tokens;

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use capability::{CapabilityQuery, CapabilitySigner};
pub use error::{AuthError, AuthzError, CapabilityRefusal, ForbiddenReason, InvalidGlob};
pub use jwt::{GeneratedKeypair, MAX_TOKEN_BYTES, PublicKey, TokenSigner, generate_keypair};
pub use rules::{Action, Authorizer, Glob, Rule, RulesAuthorizer, Target};
pub use signed_url::{TransferQuery, TransferUrls, UrlSigner, XorbUrls};
pub use static_tokens::{CasGrant, StaticToken, UserProfile};

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
    /// The scope the credential is limited to (design §5.4). Files are read
    /// and registered per scope.
    pub scope: String,
    /// The tenant the scope belongs to: the unit of isolation for xorbs and
    /// dedup under `per-tenant` visibility (design §5.4, ruling M1-R38).
    /// A JWT's optional `tenant` claim, a static token's configured tenant,
    /// or else [`default_tenant`] of the scope (ruling M1-R43).
    pub tenant: String,
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

/// What starts every repository's scope, `repo:{id}` (ruling M2-R2). Only
/// the Hub mints CAS tokens for such a scope, for a repository it
/// resolved and authorized; configuration may not name one.
pub const REPO_SCOPE_PREFIX: &str = "repo:";

/// The pseudo subject of a request without a credential (ruling M2-R3). No
/// rule names it unless the operator makes repositories public, and no
/// user credential may carry it as its subject.
pub const ANONYMOUS: &str = "anonymous";

/// The user a request acts for, which the [`Authorizer`] decides about
/// per repository (design §5.4, ruling M2-R3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPrincipal {
    /// Who the credential acts for, as the operator's identity system names
    /// them.
    pub subject: String,
    /// The display name `whoami-v2` answers: a static token's `name`, or
    /// else the subject.
    pub name: String,
    /// The address Hub commits are authored with, when the credential
    /// carries one (ruling M2-R7).
    pub email: Option<String>,
    /// String lists under free names, such as `orgs` and `groups`, which
    /// rules match and `whoami-v2` reads (`orgs`).
    pub claims: BTreeMap<String, Vec<String>>,
}

impl UserPrincipal {
    /// The principal of a request without a credential: subject and name
    /// [`ANONYMOUS`], no claims.
    #[must_use]
    pub fn anonymous() -> Self {
        Self {
            subject: ANONYMOUS.to_owned(),
            name: ANONYMOUS.to_owned(),
            email: None,
            claims: BTreeMap::new(),
        }
    }
}

/// What a bearer credential authenticates as (ruling M2-R3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// A CAS credential: a CAS token, or a static token with a scope only.
    Cas(Principal),
    /// A user credential: a static token without a scope.
    User(UserPrincipal),
    /// Both: a static token with a scope and a user identity, such as the
    /// dev token.
    Both(Principal, UserPrincipal),
}

impl Credential {
    /// The CAS credential, if there is one.
    #[must_use]
    pub fn cas(&self) -> Option<&Principal> {
        match self {
            Self::Cas(principal) | Self::Both(principal, _) => Some(principal),
            Self::User(_) => None,
        }
    }

    /// The user credential, if there is one.
    #[must_use]
    pub fn user(&self) -> Option<&UserPrincipal> {
        match self {
            Self::User(user) | Self::Both(_, user) => Some(user),
            Self::Cas(_) => None,
        }
    }
}

/// How far apart issuer and verifier clocks may be: 5 minutes. It bounds a
/// CAS token's `iat` and a signed URL's `exp` from above.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_mins(5);

/// Verifies bearer credentials: static tokens and CAS tokens.
#[derive(Debug)]
pub struct Authenticator {
    static_tokens: Vec<StaticToken>,
    jwt_keys: Vec<PublicKey>,
    max_token_lifetime: Duration,
}

impl Authenticator {
    /// The default longest CAS-token lifetime: 24 hours.
    pub const DEFAULT_MAX_TOKEN_LIFETIME: Duration = Duration::from_hours(24);

    /// The smallest accepted lifetime cap: 1 minute.
    pub const MIN_TOKEN_LIFETIME: Duration = Duration::from_mins(1);

    /// The largest accepted lifetime cap: 30 days.
    pub const MAX_TOKEN_LIFETIME: Duration = Duration::from_hours(30 * 24);

    /// An authenticator accepting `static_tokens` and CAS tokens signed by
    /// any of `jwt_keys` (several keys allow rotation, ADR 0007 §3). A CAS
    /// token whose `exp` lies more than `max_token_lifetime` after the
    /// verifier's clock is refused, so no issuer can mint a near-permanent
    /// token (ruling M1-R19).
    ///
    /// # Errors
    ///
    /// [`AuthError::LifetimeOutOfRange`] if `max_token_lifetime` is outside
    /// [`Authenticator::MIN_TOKEN_LIFETIME`] to
    /// [`Authenticator::MAX_TOKEN_LIFETIME`]: `ZERO` would refuse every
    /// token and `Duration::MAX` would disable the cap.
    /// [`AuthError::InvalidStaticToken`] if a static token's subject, scope
    /// or tenant fails the checks CAS-token claims must pass (non-empty, no
    /// control characters, scope and tenant at most [`MAX_SCOPE_BYTES`]),
    /// its name, email or claims hold control characters, a user token's
    /// subject is [`ANONYMOUS`], or it grants neither a scope nor a user
    /// identity.
    pub fn new(
        static_tokens: Vec<StaticToken>,
        jwt_keys: Vec<PublicKey>,
        max_token_lifetime: Duration,
    ) -> Result<Self, AuthError> {
        check_token_lifetime(max_token_lifetime)?;
        for (index, token) in static_tokens.iter().enumerate() {
            token
                .check()
                .map_err(|reason| AuthError::InvalidStaticToken { index, reason })?;
        }
        Ok(Self {
            static_tokens,
            jwt_keys,
            max_token_lifetime,
        })
    }

    /// Authenticates the token from an `Authorization: Bearer` header.
    ///
    /// `now` is Unix seconds. Static tokens are tried first, then CAS
    /// tokens. A CAS token is only ever a [`Credential::Cas`]; a static
    /// token is whatever it was configured as.
    ///
    /// # Errors
    ///
    /// [`AuthError::Missing`] for an empty token, [`AuthError::Expired`] for
    /// a CAS token at or past its `exp`, and [`AuthError::Invalid`] for
    /// anything else that does not verify, including a CAS token before its
    /// `nbf`, beyond the lifetime cap, or with `iat` more than
    /// [`MAX_CLOCK_SKEW`] ahead.
    pub fn authenticate(&self, bearer: &str, now: u64) -> Result<Credential, AuthError> {
        if bearer.is_empty() {
            return Err(AuthError::Missing);
        }
        if let Some(token) = static_tokens::find(&self.static_tokens, bearer) {
            return Ok(token.credential());
        }
        jwt::verify(&self.jwt_keys, bearer, now, self.max_token_lifetime).map(Credential::Cas)
    }
}

/// The longest accepted scope or tenant, in bytes.
pub const MAX_SCOPE_BYTES: usize = 1024;

/// What starts the tenant of a credential that names none (ruling M1-R43).
/// No explicit tenant may start with it, so a scope's name can never put a
/// token into a tenant someone named explicitly.
pub const DEFAULT_TENANT_PREFIX: &str = "scope:";

/// The tenant of a credential for `scope` that names no tenant:
/// [`DEFAULT_TENANT_PREFIX`] and the scope, so each such scope is a tenant
/// of its own.
#[must_use]
pub fn default_tenant(scope: &str) -> String {
    format!("{DEFAULT_TENANT_PREFIX}{scope}")
}

/// Checks an explicit tenant as [`check_identity`] checks a scope (ruling
/// M1-R38): something other than whitespace, at most [`MAX_SCOPE_BYTES`],
/// and none of the [`is_disallowed`] characters. It may not start with
/// [`DEFAULT_TENANT_PREFIX`], which only defaulted tenants carry (ruling
/// M1-R43).
pub(crate) fn check_tenant(tenant: &str) -> Result<(), &'static str> {
    if tenant.trim().is_empty() {
        return Err("the tenant is empty or only whitespace");
    }
    if tenant.starts_with(DEFAULT_TENANT_PREFIX) {
        return Err(
            "the tenant starts with `scope:`, which is reserved for tenants defaulted from a scope",
        );
    }
    if tenant.len() > MAX_SCOPE_BYTES {
        return Err("the tenant is longer than 1024 bytes");
    }
    if tenant.chars().any(is_disallowed) {
        return Err("the tenant contains control, format or line-separator characters");
    }
    Ok(())
}

/// Checks a principal's subject and scope (ruling M1-R19): both must
/// contain something other than whitespace, and neither may contain
/// characters that can forge or disguise log lines or widen a scope match
/// ([`is_disallowed`]). The scope is at most [`MAX_SCOPE_BYTES`].
pub(crate) fn check_identity(subject: &str, scope: &str) -> Result<(), &'static str> {
    check_subject(subject)?;
    check_scope(scope)
}

/// Checks a scope as [`check_identity`] does.
pub(crate) fn check_scope(scope: &str) -> Result<(), &'static str> {
    if scope.trim().is_empty() {
        return Err("the scope is empty or only whitespace");
    }
    if scope.len() > MAX_SCOPE_BYTES {
        return Err("the scope is longer than 1024 bytes");
    }
    if scope.chars().any(is_disallowed) {
        return Err("the scope contains control, format or line-separator characters");
    }
    Ok(())
}

/// Checks a subject as every credential's subject is checked: something
/// other than whitespace, and none of the characters that can forge or
/// disguise log lines (ruling M1-R19). Configuration checks rule subjects
/// with it, so a rule can only name a subject a credential can carry.
///
/// # Errors
///
/// What is wrong with `subject`; never the subject itself.
pub fn check_subject(subject: &str) -> Result<(), &'static str> {
    if subject.trim().is_empty() {
        return Err("the subject is empty or only whitespace");
    }
    if subject.chars().any(is_disallowed) {
        return Err("the subject contains control, format or line-separator characters");
    }
    Ok(())
}

/// Control characters (`Cc`), the line and paragraph separators (`Zl`,
/// `Zp`), and the invisible or bidirectional format characters (`Cf`) that
/// can reorder or hide text: zero-width and directional marks, bidi
/// embeddings, overrides and isolates (the "Trojan Source" set, CVE-2021-42574),
/// invisible operators, the word joiner and the byte-order mark.
pub(crate) fn is_disallowed(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{2069}'
                | '\u{FEFF}'
        )
}

pub(crate) fn check_token_lifetime(lifetime: Duration) -> Result<(), AuthError> {
    check_lifetime(
        "maximum CAS-token lifetime",
        lifetime,
        Authenticator::MIN_TOKEN_LIFETIME,
        Authenticator::MAX_TOKEN_LIFETIME,
    )
}

pub(crate) fn check_lifetime(
    setting: &'static str,
    lifetime: Duration,
    min: Duration,
    max: Duration,
) -> Result<(), AuthError> {
    if (min..=max).contains(&lifetime) {
        Ok(())
    } else {
        Err(AuthError::LifetimeOutOfRange {
            setting,
            lifetime,
            min,
            max,
        })
    }
}
