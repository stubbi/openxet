use std::fmt;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{Mode, Principal};

/// A long-lived bearer token from configuration (ADR 0007 §5).
///
/// Only the token's SHA-256 is held. A presented token is hashed and compared
/// against every configured hash in constant time with no early exit, so
/// response timing reveals nothing about how much of a guess matched: the
/// comparison runs on uniformly distributed digests, never on the secret.
#[derive(Clone)]
pub struct StaticToken {
    /// SHA-256 of the token's UTF-8 bytes; see [`StaticToken::digest`].
    pub sha256: [u8; 32],
    /// The scope the token is limited to.
    pub scope: String,
    /// What the token may do within `scope`.
    pub mode: Mode,
    /// Who the token acts for.
    pub subject: String,
    /// The tenant `scope` belongs to (ruling M1-R38); `None` means the scope
    /// itself.
    pub tenant: Option<String>,
}

impl StaticToken {
    /// The SHA-256 of `token`, the form in which a static token is stored.
    #[must_use]
    pub fn digest(token: &str) -> [u8; 32] {
        Sha256::digest(token.as_bytes()).into()
    }

    pub(crate) fn principal(&self) -> Principal {
        Principal {
            subject: self.subject.clone(),
            scope: self.scope.clone(),
            tenant: self.tenant.clone().unwrap_or_else(|| self.scope.clone()),
            mode: self.mode,
        }
    }
}

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticToken")
            .field("sha256", &"<redacted>")
            .field("scope", &self.scope)
            .field("mode", &self.mode)
            .field("subject", &self.subject)
            .field("tenant", &self.tenant)
            .finish()
    }
}

/// The configured token whose hash equals `presented`'s, comparing against
/// every entry so the time taken does not depend on which one matched.
pub(crate) fn find<'a>(tokens: &'a [StaticToken], presented: &str) -> Option<&'a StaticToken> {
    let digest = StaticToken::digest(presented);
    let mut found = None;
    for token in tokens {
        if bool::from(token.sha256.ct_eq(&digest)) {
            found = Some(token);
        }
    }
    found
}
