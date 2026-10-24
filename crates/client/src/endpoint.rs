//! Where to transfer and with which credential.

use std::fmt;
use std::net::IpAddr;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::ClientError;

/// The `exp` handed to the client core for a token that never expires.
/// It must be an integer, and `hf_xet` users pass `2**62` for static tokens
/// (S10 "Auth"); openxet does the same.
pub const STATIC_TOKEN_EXP: u64 = 1 << 62;

/// How long before its `exp` a token counts as expired. xet-client refreshes
/// a token this early (`REFRESH_BUFFER_SEC`, `cas_client/auth.rs`); with no
/// refresher it would fail mid-transfer, so openxet fails before starting.
pub const EXPIRY_MARGIN_SECS: u64 = 30;

/// A CAS endpoint and the bearer token for it.
///
/// Invariant: `Debug` never shows the token.
#[derive(Clone)]
pub struct Endpoint {
    /// The CAS base URL, without a trailing slash: the client core appends
    /// `/v1/...` to it (S10 "Conventions for every route").
    pub url: String,
    pub token: String,
    /// The token's expiry in Unix seconds; [`STATIC_TOKEN_EXP`] for a token
    /// that has none.
    pub token_exp: u64,
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("url", &self.url)
            .field("token", &"<redacted>")
            .field("token_exp", &self.token_exp)
            .finish()
    }
}

impl Endpoint {
    /// Checks `url` and reads the token's expiry: a JWT's `exp` claim, read
    /// without verifying the signature (the server verifies), or
    /// [`STATIC_TOKEN_EXP`] for any other token.
    ///
    /// # Errors
    ///
    /// [`ClientError::InvalidEndpoint`] unless `url` is an absolute `http`
    /// or `https` URL without credentials, a query or a fragment.
    pub fn new(url: &str, token: String) -> Result<Self, ClientError> {
        let url = checked_url(url)?;
        let token_exp = jwt_exp(&token).unwrap_or(STATIC_TOKEN_EXP);
        Ok(Self {
            url,
            token,
            token_exp,
        })
    }

    /// Whether the token crosses a network unencrypted: a plain `http://`
    /// endpoint that isn't on this host (`localhost`, `127.0.0.0/8` or
    /// `::1`). `hf_xet` sends it all the same; a caller may warn.
    #[must_use]
    pub fn token_in_clear(&self) -> bool {
        let Ok(uri) = self.url.parse::<http::Uri>() else {
            return false;
        };
        if uri.scheme_str() != Some("http") {
            return false;
        }
        let host = uri.host().unwrap_or_default();
        let host = host.trim_start_matches('[').trim_end_matches(']');
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback());
        !loopback
    }

    /// Fails if the token expires within [`EXPIRY_MARGIN_SECS`].
    pub(crate) fn check_expiry(&self) -> Result<(), ClientError> {
        if expires_soon(self.token_exp) {
            return Err(ClientError::TokenExpired {
                exp: self.token_exp,
            });
        }
        Ok(())
    }
}

/// Whether a token with expiry `exp` expires within [`EXPIRY_MARGIN_SECS`].
pub(crate) fn expires_soon(exp: u64) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    exp <= now.saturating_add(EXPIRY_MARGIN_SECS)
}

fn checked_url(raw: &str) -> Result<String, ClientError> {
    let invalid = |reason: &'static str| ClientError::InvalidEndpoint { reason };
    let uri: http::Uri = raw.parse().map_err(|_| invalid("it is not a URL"))?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) {
        return Err(invalid("it must start with http:// or https://"));
    }
    let Some(authority) = uri.authority() else {
        return Err(invalid("it names no host"));
    };
    if authority.as_str().contains('@') {
        return Err(invalid(
            "it holds credentials; pass the token with OPENXET_TOKEN instead",
        ));
    }
    if uri.query().is_some() || raw.contains('#') {
        return Err(invalid("it must not have a query or a fragment"));
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

/// A JWT's `exp` claim: three base64url segments whose middle one is a JSON
/// object with an integer `exp`.
fn jwt_exp(token: &str) -> Option<u64> {
    let mut segments = token.split('.');
    let (_header, payload, _signature) = (segments.next()?, segments.next()?, segments.next()?);
    if segments.next().is_some() {
        return None;
    }
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
    claims.get("exp")?.as_u64()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn jwt(claims: &str) -> String {
        format!(
            "{}.{}.c2ln",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"EdDSA","typ":"JWT"}"#),
            URL_SAFE_NO_PAD.encode(claims)
        )
    }

    #[test]
    fn a_static_token_never_expires() {
        let ep = Endpoint::new("http://127.0.0.1:8080", "oxt_abc".into()).unwrap();
        assert_eq!(ep.token_exp, STATIC_TOKEN_EXP);
        assert!(ep.check_expiry().is_ok());
    }

    #[test]
    fn a_jwt_expires_at_its_exp_claim_less_the_margin() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ep = Endpoint::new("http://h", jwt(&format!(r#"{{"exp":{}}}"#, now + 3600))).unwrap();
        assert_eq!(ep.token_exp, now + 3600);
        assert!(ep.check_expiry().is_ok());

        let soon = jwt(&format!(r#"{{"exp":{}}}"#, now + EXPIRY_MARGIN_SECS - 1));
        let ep = Endpoint::new("http://h", soon).unwrap();
        assert!(matches!(
            ep.check_expiry(),
            Err(ClientError::TokenExpired { .. })
        ));
    }

    #[test]
    fn a_token_that_only_looks_like_a_jwt_is_static() {
        for token in [
            "a.b.c",
            "a.b",
            &jwt(r#"{"exp":"soon"}"#),
            &format!("{}.x", jwt("{}")),
        ] {
            assert_eq!(jwt_exp(token), None, "{token}");
        }
    }

    #[test]
    fn debug_never_shows_the_token() {
        let ep = Endpoint::new("http://h", "oxt_secret-marker".into()).unwrap();
        assert!(!format!("{ep:?}").contains("secret-marker"));
    }

    #[test]
    fn the_token_travels_in_clear_only_over_http_off_this_host() {
        let in_clear = |url: &str| Endpoint::new(url, "t".into()).unwrap().token_in_clear();
        for url in [
            "http://cas.example.com",
            "http://10.0.0.5:8080",
            "http://[2001:db8::1]:8080",
            "http://localhost.example.com",
        ] {
            assert!(in_clear(url), "{url}");
        }
        for url in [
            "https://cas.example.com",
            "http://127.0.0.1:8080",
            "http://127.1.2.3",
            "http://[::1]:9000",
            "http://localhost:8080",
            "http://LOCALHOST",
        ] {
            assert!(!in_clear(url), "{url}");
        }
    }

    #[test]
    fn urls_are_checked_and_trimmed() {
        let ep = Endpoint::new("https://cas.example.com/", "t".into()).unwrap();
        assert_eq!(ep.url, "https://cas.example.com");
        assert!(Endpoint::new("http://[::1]:9000", "t".into()).is_ok());
        for bad in [
            "cas.example.com",
            "ftp://h",
            "local:///tmp/cas",
            "memory://",
            "http://user:pass@h",
            "http://h?x=1",
            "http://h#f",
        ] {
            assert!(
                matches!(
                    Endpoint::new(bad, "t".into()),
                    Err(ClientError::InvalidEndpoint { .. })
                ),
                "{bad}"
            );
        }
    }
}
