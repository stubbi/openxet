//! Capability `href`s (ADR 0003 decisions 1 and 2, ADR 0005 item 10).
//!
//! A capability is the query `exp={unix}&sig={b64url}` on a URL naming one
//! scope and one operation. `sig` is the unpadded base64url HMAC-SHA256,
//! under the transfer key, of `"openxet-capability-v1\n{scope}\n{op}\n{exp}"`
//! with `op` `read` or `write`. The signature is the whole authorization:
//! capability routes never authenticate by header.
//!
//! Transfer URLs are signed with the same key over strings that start with
//! `"xorb\n"` ([`crate::UrlSigner`]), so neither kind of signature verifies
//! as the other. `op` and `exp` hold no newline, so the string splits from
//! its end into exactly one scope, operation and expiry, whatever the scope
//! holds.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq as _;
use zeroize::Zeroize as _;

use crate::{AuthError, CapabilityRefusal, Mode};

/// The domain string that starts every capability's signed string.
const DOMAIN: &[u8] = b"openxet-capability-v1\n";

/// The query parameters of a capability `href`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityQuery {
    /// The expiry, Unix seconds; the capability is invalid from this second
    /// on.
    pub exp: u64,
    /// The unpadded base64url HMAC-SHA256 signature.
    pub sig: String,
}

impl fmt::Debug for CapabilityQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapabilityQuery")
            .field("exp", &self.exp)
            .field("sig", &"<redacted>")
            .finish()
    }
}

/// Signs and checks capabilities with the transfer key shared by every node
/// (ruling M1-R29).
pub struct CapabilitySigner {
    mac: Hmac<Sha256>,
}

impl CapabilitySigner {
    /// A signer using `key`, the transfer key that also signs transfer URLs.
    ///
    /// The local copy of `key` is wiped once the HMAC state is derived, and
    /// that state is wiped on drop (`sha2`'s `zeroize` feature).
    ///
    /// # Errors
    ///
    /// [`AuthError::InvalidKey`] if HMAC-SHA256 rejects the key, which it
    /// does for no 32-byte key.
    pub fn new(mut key: [u8; 32]) -> Result<Self, AuthError> {
        let mac = Hmac::<Sha256>::new_from_slice(&key);
        key.zeroize();
        let mac = mac.map_err(|_| AuthError::InvalidKey {
            reason: "the capability-signing key was rejected by HMAC-SHA256",
        })?;
        Ok(Self { mac })
    }

    /// The capability for `op` on `scope`, valid until `exp` (Unix seconds,
    /// exclusive).
    #[must_use]
    pub fn sign(&self, scope: &str, op: Mode, exp: u64) -> CapabilityQuery {
        CapabilityQuery {
            exp,
            sig: URL_SAFE_NO_PAD.encode(self.tag(scope, op, exp)),
        }
    }

    /// Checks that `query` is the capability for `op` on `scope` and that
    /// `now` (Unix seconds) is before its `exp`.
    ///
    /// The signature is checked first, and compared in constant time over
    /// the whole tag, so a request learns nothing about how close a forgery
    /// came, nor whether a forged one had expired.
    ///
    /// # Errors
    ///
    /// [`AuthError::CapabilityRefused`] with
    /// [`CapabilityRefusal::BadSignature`] if `sig` is not the signature of
    /// `scope`, `op` and `exp`, and with [`CapabilityRefusal::Expired`] if
    /// it is but `now >= exp`.
    pub fn verify(
        &self,
        scope: &str,
        op: Mode,
        query: &CapabilityQuery,
        now: u64,
    ) -> Result<(), AuthError> {
        let refused = |reason| AuthError::CapabilityRefused { reason };
        let presented = URL_SAFE_NO_PAD
            .decode(&query.sig)
            .map_err(|_| refused(CapabilityRefusal::BadSignature))?;
        let expected = self.tag(scope, op, query.exp);
        if !bool::from(expected.as_slice().ct_eq(&presented)) {
            return Err(refused(CapabilityRefusal::BadSignature));
        }
        if now >= query.exp {
            return Err(refused(CapabilityRefusal::Expired));
        }
        Ok(())
    }

    fn tag(&self, scope: &str, op: Mode, exp: u64) -> [u8; 32] {
        let mut mac = self.mac.clone();
        mac.update(DOMAIN);
        mac.update(scope.as_bytes());
        mac.update(match op {
            Mode::Read => b"\nread\n".as_slice(),
            Mode::Write => b"\nwrite\n".as_slice(),
        });
        mac.update(itoa::Buffer::new().format(exp).as_bytes());
        mac.finalize().into_bytes().into()
    }
}

impl fmt::Debug for CapabilitySigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapabilitySigner")
            .field("mac", &"<redacted>")
            .finish()
    }
}
