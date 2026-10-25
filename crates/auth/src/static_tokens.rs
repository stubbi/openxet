use std::collections::BTreeMap;
use std::fmt;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::{ANONYMOUS, Credential, Mode, Principal, UserPrincipal};

/// A long-lived bearer token from configuration (ADR 0007 §5).
///
/// Only the token's SHA-256 is held. A presented token is hashed and compared
/// against every configured hash in constant time with no early exit, so
/// response timing reveals nothing about how much of a guess matched: the
/// comparison runs on uniformly distributed digests, never on the secret.
///
/// A token grants a CAS scope, a user identity, or both (ruling M2-R3), and
/// authenticates as [`Credential::Cas`], [`Credential::User`] or
/// [`Credential::Both`] accordingly. One that grants neither is refused
/// by [`Authenticator::new`](crate::Authenticator::new).
#[derive(Clone)]
pub struct StaticToken {
    /// SHA-256 of the token's UTF-8 bytes; see [`StaticToken::digest`].
    pub sha256: [u8; 32],
    /// Who the token acts for.
    pub subject: String,
    /// The CAS scope the token grants, if any.
    pub cas: Option<CasGrant>,
    /// The user identity the token carries, if any.
    pub user: Option<UserProfile>,
}

/// What a static token may do on CAS routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CasGrant {
    /// The scope the token is limited to.
    pub scope: String,
    /// What the token may do within `scope`.
    pub mode: Mode,
    /// The tenant `scope` belongs to (ruling M1-R38); `None` means
    /// [`default_tenant`](crate::default_tenant) of the scope (ruling M1-R43).
    pub tenant: Option<String>,
}

/// The user identity a static token carries beyond its subject.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserProfile {
    /// The display name; `None` means the subject.
    pub name: Option<String>,
    /// The address Hub commits are authored with.
    pub email: Option<String>,
    /// String lists under free names, such as `orgs` and `groups`.
    pub claims: BTreeMap<String, Vec<String>>,
}

impl StaticToken {
    /// The SHA-256 of `token`, the form in which a static token is stored.
    #[must_use]
    pub fn digest(token: &str) -> [u8; 32] {
        Sha256::digest(token.as_bytes()).into()
    }

    /// What the token authenticates as. A token granting neither a scope
    /// nor a user identity, which [`Authenticator::new`](crate::Authenticator::new)
    /// refuses, would be a user credential without claims.
    pub(crate) fn credential(&self) -> Credential {
        let cas = self.cas.as_ref().map(|grant| Principal {
            subject: self.subject.clone(),
            scope: grant.scope.clone(),
            tenant: grant
                .tenant
                .clone()
                .unwrap_or_else(|| crate::default_tenant(&grant.scope)),
            mode: grant.mode,
        });
        let user = |profile: &UserProfile| UserPrincipal {
            subject: self.subject.clone(),
            name: profile.name.clone().unwrap_or_else(|| self.subject.clone()),
            email: profile.email.clone(),
            claims: profile.claims.clone(),
        };
        match (cas, &self.user) {
            (Some(cas), None) => Credential::Cas(cas),
            (Some(cas), Some(profile)) => Credential::Both(cas, user(profile)),
            (None, profile) => {
                Credential::User(user(profile.as_ref().unwrap_or(&UserProfile::default())))
            }
        }
    }

    /// The checks [`Authenticator::new`](crate::Authenticator::new) applies.
    pub(crate) fn check(&self) -> Result<(), &'static str> {
        crate::check_subject(&self.subject)?;
        if let Some(grant) = &self.cas {
            crate::check_identity(&self.subject, &grant.scope)?;
            grant
                .tenant
                .as_deref()
                .map_or(Ok(()), crate::check_tenant)?;
        }
        if self.cas.is_none() && self.user.is_none() {
            return Err("the token grants neither a scope nor a user identity");
        }
        if let Some(profile) = &self.user {
            // Only a request without any credential is anonymous, so an
            // `anonymous` rule can't be mistaken for a grant to one token.
            if self.subject == ANONYMOUS {
                return Err(
                    "the subject `anonymous` is reserved for requests without a credential",
                );
            }
            profile.check()?;
        }
        Ok(())
    }
}

impl UserProfile {
    fn check(&self) -> Result<(), &'static str> {
        let clean = |text: &str| !text.chars().any(crate::is_disallowed);
        if self
            .name
            .as_deref()
            .is_some_and(|name| name.trim().is_empty() || !clean(name))
        {
            return Err(
                "the name is empty or contains control, format or line-separator characters",
            );
        }
        if self
            .email
            .as_deref()
            .is_some_and(|email| email.trim().is_empty() || !clean(email))
        {
            return Err(
                "the email is empty or contains control, format or line-separator characters",
            );
        }
        for (name, values) in &self.claims {
            if name.trim().is_empty() || !clean(name) {
                return Err(
                    "a claim name is empty or contains control, format or line-separator characters",
                );
            }
            if !values.iter().all(|value| clean(value)) {
                return Err("a claim value contains control, format or line-separator characters");
            }
        }
        Ok(())
    }
}

impl fmt::Debug for StaticToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticToken")
            .field("sha256", &"<redacted>")
            .field("subject", &self.subject)
            .field("cas", &self.cas)
            .field("user", &self.user)
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
