//! Range-bound signed transfer URLs (design §5.5, §5.9 requirement 1; ADR
//! 0002; ruling M1-R3).
//!
//! A URL has the form
//! `{base}/v1/transfer/xorbs/{xorb_hex}?range={start}-{end}&exp={unix}&sig={b64url}`
//! where `sig` is the unpadded base64url HMAC-SHA256 of
//! `"xorb\n{xorb_hex}\n{start}-{end}\n{exp}"`. It is valid while
//! `now < exp`, and only when fetched with `Range: bytes={start}-{end}`
//! exactly, so a leaked URL yields that one range and nothing more.

use std::fmt;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit as _, Mac as _};
use openxet_protocol::{MerkleHash, hash_to_hex};
use serde::Deserialize;
use sha2::Sha256;
use url::Url;
use zeroize::Zeroize as _;

use crate::{AuthError, ForbiddenReason};

/// The query parameters of a signed transfer URL, as the CAS route
/// extracts them.
#[derive(Clone, Deserialize)]
pub struct TransferQuery {
    /// The signed range, `{start}-{end}` with `end` inclusive.
    pub range: String,
    /// The expiry, Unix seconds; the URL is invalid from this second on.
    pub exp: u64,
    /// The unpadded base64url HMAC-SHA256 signature.
    pub sig: String,
}

impl fmt::Debug for TransferQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransferQuery")
            .field("range", &self.range)
            .field("exp", &self.exp)
            .field("sig", &"<redacted>")
            .finish()
    }
}

/// Signs and checks transfer URLs with an HMAC key shared by every node.
pub struct UrlSigner {
    mac: Hmac<Sha256>,
    lifetime: Duration,
}

impl UrlSigner {
    /// The default URL lifetime: 60 minutes.
    pub const DEFAULT_LIFETIME: Duration = Duration::from_hours(1);

    /// The shortest accepted URL lifetime: 30 minutes, derived in ADR 0002
    /// §5 from how long `hf_xet` holds a URL before its last use.
    pub const MIN_LIFETIME: Duration = Duration::from_mins(30);

    /// The longest accepted URL lifetime: 24 hours (ruling M1-R19), so a
    /// leaked URL stays usable for a bounded time.
    pub const MAX_LIFETIME: Duration = Duration::from_hours(24);

    /// A signer using `key`, issuing URLs valid for `lifetime`.
    ///
    /// The local copy of `key` is wiped once the HMAC state is derived, and
    /// that state is wiped on drop (`sha2`'s `zeroize` feature). Callers
    /// should hold their own copy in [`zeroize::Zeroizing`].
    ///
    /// # Errors
    ///
    /// [`AuthError::LifetimeOutOfRange`] if `lifetime` is outside
    /// [`UrlSigner::MIN_LIFETIME`] to [`UrlSigner::MAX_LIFETIME`].
    pub fn new(mut key: [u8; 32], lifetime: Duration) -> Result<Self, AuthError> {
        let mac = Hmac::<Sha256>::new_from_slice(&key);
        key.zeroize();
        if !(Self::MIN_LIFETIME..=Self::MAX_LIFETIME).contains(&lifetime) {
            return Err(AuthError::LifetimeOutOfRange {
                lifetime_secs: lifetime.as_secs(),
            });
        }
        let mac = mac.map_err(|_| AuthError::InvalidKey {
            reason: "the URL-signing key was rejected by HMAC-SHA256",
        })?;
        Ok(Self { mac, lifetime })
    }

    /// A URL under `base` serving bytes `start..=end_incl` of `xorb`, valid
    /// for this signer's lifetime from `now` (Unix seconds). Any path in
    /// `base` is kept as a prefix; its query and fragment are dropped.
    ///
    /// # Errors
    ///
    /// [`AuthError::ReversedRange`] if `start > end_incl`.
    pub fn sign_xorb_range(
        &self,
        base: &Url,
        xorb: &MerkleHash,
        start: u64,
        end_incl: u64,
        now: u64,
    ) -> Result<Url, AuthError> {
        if start > end_incl {
            return Err(AuthError::ReversedRange {
                start,
                end: end_incl,
            });
        }
        let xorb_hex = hash_to_hex(xorb);
        let exp = now.saturating_add(self.lifetime.as_secs());
        let sig = URL_SAFE_NO_PAD.encode(
            self.mac_for(&xorb_hex, start, end_incl, exp)
                .finalize()
                .into_bytes(),
        );

        let mut url = base.clone();
        url.set_path(&format!(
            "{}/v1/transfer/xorbs/{xorb_hex}",
            base.path().trim_end_matches('/')
        ));
        url.set_fragment(None);
        url.set_query(None);
        url.query_pairs_mut()
            .append_pair("range", &format!("{start}-{end_incl}"))
            .append_pair("exp", &exp.to_string())
            .append_pair("sig", &sig);
        Ok(url)
    }

    /// Checks a request for `xorb` carrying `query` and `range_header`,
    /// returning the inclusive byte range to serve.
    ///
    /// # Errors
    ///
    /// - [`AuthError::BadRange`] if `range_header` is absent (HTTP `400`).
    /// - [`AuthError::Forbidden`] (HTTP `403`) if the signature does not
    ///   match (compared in constant time), the URL is at or past `exp`, its
    ///   `exp` is more than this signer's lifetime plus
    ///   [`MAX_CLOCK_SKEW`](crate::MAX_CLOCK_SKEW) ahead, or `range_header`
    ///   is not exactly `bytes={start}-{end}`.
    pub fn check(
        &self,
        xorb: &MerkleHash,
        query: &TransferQuery,
        range_header: Option<&str>,
        now: u64,
    ) -> Result<(u64, u64), AuthError> {
        let forbidden = |reason| AuthError::Forbidden { reason };
        let range_header = range_header.ok_or(AuthError::BadRange)?;
        let (start, end) =
            parse_range(&query.range).ok_or(forbidden(ForbiddenReason::BadSignature))?;
        let sig = URL_SAFE_NO_PAD
            .decode(&query.sig)
            .map_err(|_| forbidden(ForbiddenReason::BadSignature))?;
        self.mac_for(&hash_to_hex(xorb), start, end, query.exp)
            .verify_slice(&sig)
            .map_err(|_| forbidden(ForbiddenReason::BadSignature))?;
        if now >= query.exp {
            return Err(forbidden(ForbiddenReason::Expired));
        }
        let latest_exp = now
            .saturating_add(self.lifetime.as_secs())
            .saturating_add(crate::MAX_CLOCK_SKEW.as_secs());
        if query.exp > latest_exp {
            return Err(forbidden(ForbiddenReason::ExpiryBeyondLifetime));
        }
        if range_header.strip_prefix("bytes=") != Some(query.range.as_str()) {
            return Err(forbidden(ForbiddenReason::RangeMismatch));
        }
        Ok((start, end))
    }

    fn mac_for(&self, xorb_hex: &str, start: u64, end: u64, exp: u64) -> Hmac<Sha256> {
        let mut mac = self.mac.clone();
        mac.update(format!("xorb\n{xorb_hex}\n{start}-{end}\n{exp}").as_bytes());
        mac
    }
}

impl fmt::Debug for UrlSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UrlSigner")
            .field("mac", &"<redacted>")
            .field("lifetime", &self.lifetime)
            .finish()
    }
}

/// Parses `{start}-{end}` in the canonical decimal form the signer writes,
/// with `start <= end`.
fn parse_range(range: &str) -> Option<(u64, u64)> {
    let (start, end) = range.split_once('-')?;
    let (start, end) = (start.parse::<u64>().ok()?, end.parse::<u64>().ok()?);
    (start <= end && format!("{start}-{end}") == range).then_some((start, end))
}
