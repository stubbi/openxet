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
        crate::check_lifetime(
            "signed-URL lifetime",
            lifetime,
            Self::MIN_LIFETIME,
            Self::MAX_LIFETIME,
        )?;
        let mac = mac.map_err(|_| AuthError::InvalidKey {
            reason: "the URL-signing key was rejected by HMAC-SHA256",
        })?;
        Ok(Self { mac, lifetime })
    }

    /// A URL under `base` serving bytes `start..=end_incl` of `xorb`, valid
    /// for this signer's lifetime from `now` (Unix seconds). Any path in
    /// `base` is kept as a prefix; its query and fragment are dropped.
    ///
    /// To sign many URLs, [`UrlSigner::transfer_urls`] does the per-base and
    /// per-xorb work once.
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
        let mut url = String::new();
        self.transfer_urls(base, now)
            .xorb(xorb)
            .push_signed(start, end_incl, &mut url)?;
        // A serialized `Url`'s path extended by hex, and a query of digits,
        // `-`, `&`, `=` and base64url: a URL that parses back unchanged.
        Ok(Url::parse(&url).unwrap_or_else(|_| unreachable!("a signed URL always parses")))
    }

    /// Signs transfer URLs under `base`, valid for this signer's lifetime
    /// from `now`: the URLs [`UrlSigner::sign_xorb_range`] returns, byte for
    /// byte, with the base serialized once and each xorb's part of the MAC
    /// computed once. A reconstruction signs one URL per fetch descriptor,
    /// up to 100k of them.
    #[must_use]
    pub fn transfer_urls(&self, base: &Url, now: u64) -> TransferUrls<'_> {
        let mut prefix = base.clone();
        prefix.set_path(&format!(
            "{}/v1/transfer/xorbs/",
            base.path().trim_end_matches('/')
        ));
        prefix.set_fragment(None);
        prefix.set_query(None);
        let exp = now.saturating_add(self.lifetime.as_secs());
        TransferUrls {
            mac: &self.mac,
            prefix: prefix.into(),
            exp: itoa::Buffer::new().format(exp).to_owned(),
        }
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
        let mut mac = xorb_mac(&self.mac, xorb_hex);
        range_mac(
            &mut mac,
            itoa::Buffer::new().format(start),
            itoa::Buffer::new().format(end),
            itoa::Buffer::new().format(exp),
        );
        mac
    }
}

/// The MAC over the canonical string's first part, `"xorb\n{xorb_hex}\n"`.
/// [`range_mac`] adds the rest; HMAC is streaming, so the parts MAC the same
/// as the whole string.
fn xorb_mac(keyed: &Hmac<Sha256>, xorb_hex: &str) -> Hmac<Sha256> {
    let mut mac = keyed.clone();
    mac.update(b"xorb\n");
    mac.update(xorb_hex.as_bytes());
    mac.update(b"\n");
    mac
}

/// Adds the canonical string's rest, `"{start}-{end}\n{exp}"`, to `mac`.
fn range_mac(mac: &mut Hmac<Sha256>, start: &str, end: &str, exp: &str) {
    mac.update(start.as_bytes());
    mac.update(b"-");
    mac.update(end.as_bytes());
    mac.update(b"\n");
    mac.update(exp.as_bytes());
}

/// Signs many transfer URLs under one base and expiry
/// ([`UrlSigner::transfer_urls`]).
pub struct TransferUrls<'a> {
    mac: &'a Hmac<Sha256>,
    /// `{base}/v1/transfer/xorbs/`, serialized.
    prefix: String,
    /// The expiry, in decimal.
    exp: String,
}

impl TransferUrls<'_> {
    /// The signer of `xorb`'s URLs.
    #[must_use]
    pub fn xorb(&self, xorb: &MerkleHash) -> XorbUrls<'_> {
        let hex = hash_to_hex(xorb);
        XorbUrls {
            mac: xorb_mac(self.mac, &hex),
            urls: self,
            hex,
        }
    }
}

impl fmt::Debug for TransferUrls<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransferUrls")
            .field("mac", &"<redacted>")
            .field("prefix", &self.prefix)
            .field("exp", &self.exp)
            .finish()
    }
}

/// Signs one xorb's transfer URLs ([`TransferUrls::xorb`]), keeping the MAC
/// state over the xorb's part of the canonical string so each URL hashes
/// only its range and expiry.
pub struct XorbUrls<'a> {
    mac: Hmac<Sha256>,
    urls: &'a TransferUrls<'a>,
    hex: String,
}

impl XorbUrls<'_> {
    /// Appends the URL serving bytes `start..=end_incl` of the xorb to
    /// `out`: the URL [`UrlSigner::sign_xorb_range`] returns, as a string.
    ///
    /// # Errors
    ///
    /// [`AuthError::ReversedRange`] if `start > end_incl`; `out` is then
    /// unchanged.
    pub fn push_signed(
        &self,
        start: u64,
        end_incl: u64,
        out: &mut String,
    ) -> Result<(), AuthError> {
        if start > end_incl {
            return Err(AuthError::ReversedRange {
                start,
                end: end_incl,
            });
        }
        let (mut a, mut b) = (itoa::Buffer::new(), itoa::Buffer::new());
        let (start, end) = (a.format(start), b.format(end_incl));
        let mut mac = self.mac.clone();
        range_mac(&mut mac, start, end, &self.urls.exp);
        // The query as `Url::query_pairs_mut` form-encodes it: digits, `-`
        // and base64url pass through unchanged.
        out.push_str(&self.urls.prefix);
        out.push_str(&self.hex);
        out.push_str("?range=");
        out.push_str(start);
        out.push('-');
        out.push_str(end);
        out.push_str("&exp=");
        out.push_str(&self.urls.exp);
        out.push_str("&sig=");
        URL_SAFE_NO_PAD.encode_string(mac.finalize().into_bytes(), out);
        Ok(())
    }
}

impl fmt::Debug for XorbUrls<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XorbUrls")
            .field("mac", &"<redacted>")
            .field("xorb", &self.hex)
            .finish_non_exhaustive()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A reversed range with a valid MAC, which `sign_xorb_range` refuses to
    /// mint, must still be refused by `check`: the `start <= end` guard in
    /// `parse_range` is the only thing that stops it.
    #[test]
    fn check_refuses_a_validly_signed_reversed_range() {
        let signer = UrlSigner::new([3; 32], UrlSigner::DEFAULT_LIFETIME).expect("valid");
        let xorb = openxet_protocol::chunk_hash(b"reversed");
        let exp = 1_000;
        let query = TransferQuery {
            range: "9-7".to_owned(),
            exp,
            sig: URL_SAFE_NO_PAD.encode(
                signer
                    .mac_for(&hash_to_hex(&xorb), 9, 7, exp)
                    .finalize()
                    .into_bytes(),
            ),
        };
        assert_eq!(
            signer.check(&xorb, &query, Some("bytes=9-7"), 0),
            Err(AuthError::Forbidden {
                reason: ForbiddenReason::BadSignature
            })
        );
    }
}
