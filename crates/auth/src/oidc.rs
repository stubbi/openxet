//! OIDC: bearer tokens from the operator's identity providers (design
//! §5.4, ruling M3-R7, ADR 0016 item 1).
//!
//! An issuer is discovered once from
//! `{issuer}/.well-known/openid-configuration` (OIDC Discovery
//! 1.0 §4), whose `issuer` must equal the configured one exactly (§4.3).
//! Its JWKS is cached and refreshed every [`JWKS_REFRESH_INTERVAL`] by
//! [`OidcIssuer::keep_fresh`], and on a `kid` the cache doesn't hold at
//! most once per [`UNKNOWN_KID_REFRESH_INTERVAL`]. A refresh that fails
//! keeps the last keys, which verify until they are older than
//! `jwks_max_stale`; past that a token gets [`AuthError::Unavailable`],
//! never an unverified accept (design §5.9 requirement 3).
//!
//! A token verifies only if all of these hold:
//! - it is at most [`MAX_TOKEN_BYTES`];
//! - its header's `alg` is RS256, PS256, ES256, ES384 or `EdDSA`, and exactly
//!   the `alg` of the JWKS key its `kid` names, so `none`, HMAC and a key
//!   used under another algorithm are refused (RFC 8725 §3.1); a `crit`
//!   header is refused (RFC 7515 §4.1.11);
//! - the signature verifies under that key;
//! - `iss` is the configured issuer, and `aud` names a configured audience;
//! - `now < exp`, exactly, and `nbf` and `iat` are at most
//!   [`MAX_CLOCK_SKEW`] ahead;
//! - the subject claim is a string that makes a valid subject.
//!
//! The principal's subject is `{name}:{sub}`, so two issuers never share a
//! subject. An OIDC token is a user credential, never a CAS credential.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::jwk::{AlgorithmParameters, EllipticCurve, Jwk, KeyAlgorithm, PublicKeyUse};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use serde_json::{Map, Value};
use tokio::sync::Mutex;
use url::{Host, Url};

use crate::{AuthError, MAX_CLOCK_SKEW, MAX_TOKEN_BYTES, UserPrincipal};

/// How often [`OidcIssuer::keep_fresh`] fetches an issuer's JWKS: 10
/// minutes.
pub const JWKS_REFRESH_INTERVAL: Duration = Duration::from_mins(10);

/// The least time between two JWKS fetches for tokens naming a `kid` the
/// cache doesn't hold: 30 seconds, so a stream of made-up `kid`s costs the
/// issuer at most one fetch per 30 s per node.
pub const UNKNOWN_KID_REFRESH_INTERVAL: Duration = Duration::from_secs(30);

/// How long one discovery or JWKS fetch may take. An unknown `kid` waits
/// for one on the request path.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest discovery document or JWKS read: 1 MiB. Real ones are a few
/// KiB.
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;

/// The longest issuer `name`, in bytes.
const MAX_NAME_BYTES: usize = 64;

/// The signature algorithms an OIDC token may use (ruling M3-R7): never
/// `none` or HMAC, whose key would be the issuer's public key.
const ACCEPTED: [Algorithm; 5] = [
    Algorithm::RS256,
    Algorithm::PS256,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

/// One `[[auth.oidc]]` entry (ruling M3-R7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OidcConfig {
    /// The subject prefix: a principal's subject is `{name}:{sub}`.
    pub name: String,
    /// The issuer, matched exactly against the discovery document's
    /// `issuer` and every token's `iss`.
    pub issuer: String,
    /// The accepted audiences; a token's `aud` must name one.
    pub audiences: Vec<String>,
    /// The claim holding the subject; `sub` by default.
    pub subject_claim: String,
    /// The claim holding the display name; `preferred_username` by
    /// default, falling back to the subject.
    pub name_claim: String,
    /// The claim holding the email address; `email` by default.
    pub email_claim: String,
    /// String or string-list claims copied into the principal, which rules
    /// match; `orgs` feeds `whoami-v2`.
    pub claims: Vec<String>,
    /// How long the last fetched keys verify while the issuer can't be
    /// reached; 24 hours by default.
    pub jwks_max_stale: Duration,
}

impl OidcConfig {
    /// The default `subject_claim`.
    pub const DEFAULT_SUBJECT_CLAIM: &str = "sub";
    /// The default `name_claim`.
    pub const DEFAULT_NAME_CLAIM: &str = "preferred_username";
    /// The default `email_claim`.
    pub const DEFAULT_EMAIL_CLAIM: &str = "email";
    /// The default `jwks_max_stale`: 24 hours.
    pub const DEFAULT_JWKS_MAX_STALE: Duration = Duration::from_hours(24);
    /// The smallest `jwks_max_stale`: one refresh interval, 10 minutes.
    pub const MIN_JWKS_MAX_STALE: Duration = JWKS_REFRESH_INTERVAL;
    /// The largest `jwks_max_stale`: 7 days.
    pub const MAX_JWKS_MAX_STALE: Duration = Duration::from_hours(7 * 24);

    /// An issuer `name` at `issuer` for `audiences`, with every other
    /// setting at its default.
    #[must_use]
    pub fn new(name: &str, issuer: &str, audiences: &[&str]) -> Self {
        Self {
            name: name.to_owned(),
            issuer: issuer.to_owned(),
            audiences: audiences.iter().map(|&a| a.to_owned()).collect(),
            subject_claim: Self::DEFAULT_SUBJECT_CLAIM.to_owned(),
            name_claim: Self::DEFAULT_NAME_CLAIM.to_owned(),
            email_claim: Self::DEFAULT_EMAIL_CLAIM.to_owned(),
            claims: Vec::new(),
            jwks_max_stale: Self::DEFAULT_JWKS_MAX_STALE,
        }
    }

    /// Checks the entry as configuration loading and
    /// [`OidcIssuer::discover`] do.
    ///
    /// # Errors
    ///
    /// What is wrong, naming the field.
    pub fn check(&self) -> Result<(), &'static str> {
        check_name(&self.name)?;
        let issuer = Url::parse(&self.issuer).map_err(|_| "`issuer` is not a URL")?;
        check_endpoint(&issuer)?;
        if issuer.query().is_some() || issuer.fragment().is_some() {
            return Err("`issuer` has a query or fragment, which an OIDC issuer never has");
        }
        if self.audiences.is_empty() {
            return Err("`audiences` is empty; name the audience your tokens are issued for");
        }
        let names = [&self.subject_claim, &self.name_claim, &self.email_claim];
        if self.audiences.iter().any(|a| a.trim().is_empty())
            || names.iter().any(|c| c.trim().is_empty())
            || self.claims.iter().any(|c| c.trim().is_empty())
        {
            return Err("an audience or claim name is empty");
        }
        if !(Self::MIN_JWKS_MAX_STALE..=Self::MAX_JWKS_MAX_STALE).contains(&self.jwks_max_stale) {
            return Err("`jwks_max_stale` is outside 10m to 7d");
        }
        Ok(())
    }
}

/// An issuer's `name`: a short identifier that can't make two issuers'
/// subjects meet. `delegated` is reserved for delegated tokens (ADR 0007),
/// `anonymous` for requests without a credential (ruling M2-R3).
fn check_name(name: &str) -> Result<(), &'static str> {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    if !first_ok
        || name.len() > MAX_NAME_BYTES
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err(
            "`name` must be 1 to 64 ASCII letters, digits, `-`, `_` or `.`, starting with a letter or digit",
        );
    }
    if name == "delegated" || name == crate::ANONYMOUS {
        return Err("`name` is reserved: `delegated` and `anonymous` name OpenXet's own subjects");
    }
    Ok(())
}

/// An issuer or JWKS URL: `https`, or `http` only to a loopback address,
/// so keys never travel where they could be swapped.
fn check_endpoint(url: &Url) -> Result<(), &'static str> {
    let loopback = match url.host() {
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        Some(Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    match url.scheme() {
        "https" if url.host().is_some() => Ok(()),
        "http" if loopback => Ok(()),
        _ => Err("the URL must be `https://`, or `http://` to a loopback address"),
    }
}

/// An identity provider whose tokens authenticate users.
pub struct OidcIssuer {
    cfg: OidcConfig,
    jwks_uri: Url,
    http: reqwest::Client,
    jwks: ArcSwap<Jwks>,
    refresh: Mutex<Refresh>,
}

/// The cached keys, by `kid`, and when they were fetched (Unix seconds).
struct Jwks {
    keys: BTreeMap<String, IssuerKey>,
    fetched_at: u64,
}

#[derive(Clone)]
struct IssuerKey {
    alg: Algorithm,
    key: DecodingKey,
}

/// The last JWKS fetch: when it was tried (Unix seconds) and whether it
/// failed.
#[derive(Default)]
struct Refresh {
    last_attempt: u64,
    failed: bool,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    jwks_uri: String,
}

impl fmt::Debug for OidcIssuer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcIssuer")
            .field("name", &self.cfg.name)
            .field("issuer", &self.cfg.issuer)
            .field("jwks_uri", &self.jwks_uri.as_str())
            .finish_non_exhaustive()
    }
}

impl OidcIssuer {
    /// Discovers the issuer `cfg` names and fetches its JWKS, with `http`.
    ///
    /// # Errors
    ///
    /// [`AuthError::Oidc`] if `cfg` fails [`OidcConfig::check`], the
    /// discovery document or JWKS can't be fetched or read, the document
    /// names another issuer, or the JWKS holds no key a token could be
    /// verified with.
    pub async fn discover(cfg: OidcConfig, http: reqwest::Client) -> Result<Self, AuthError> {
        let fail = |reason: String| AuthError::Oidc {
            name: cfg.name.clone(),
            reason,
        };
        cfg.check().map_err(|reason| fail(reason.to_owned()))?;
        let url = format!(
            "{}/.well-known/openid-configuration",
            cfg.issuer.trim_end_matches('/')
        );
        let url = Url::parse(&url).map_err(|err| fail(format!("{url} is not a URL: {err}")))?;
        let body = fetch(&http, &url).await.map_err(fail)?;
        let doc: Discovery = serde_json::from_slice(&body).map_err(|err| {
            fail(format!(
                "the discovery document at {url} is not JSON with `issuer` and `jwks_uri`: {err}"
            ))
        })?;
        if doc.issuer != cfg.issuer {
            return Err(fail(format!(
                "the discovery document names the issuer {:?}, not {:?}; set `issuer` to \
                 exactly what the provider's tokens carry in `iss`",
                doc.issuer, cfg.issuer
            )));
        }
        let jwks_uri = Url::parse(&doc.jwks_uri)
            .map_err(|_| fail(format!("its `jwks_uri` {:?} is not a URL", doc.jwks_uri)))?;
        check_endpoint(&jwks_uri).map_err(|reason| fail(format!("its `jwks_uri`: {reason}")))?;
        let keys = fetch_keys(&http, &jwks_uri).await.map_err(fail)?;
        if keys.is_empty() {
            return Err(fail(format!(
                "the JWKS at {jwks_uri} holds no signing key with a `kid` for RS256, PS256, \
                 ES256, ES384 or EdDSA"
            )));
        }
        Ok(Self {
            jwks: ArcSwap::from_pointee(Jwks {
                keys,
                fetched_at: unix_now(),
            }),
            refresh: Mutex::new(Refresh::default()),
            cfg,
            jwks_uri,
            http,
        })
    }

    /// The configured `name`, every subject's prefix.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.cfg.name
    }

    /// The issuer, as tokens carry it in `iss`.
    #[must_use]
    pub fn issuer(&self) -> &str {
        &self.cfg.issuer
    }

    /// Refreshes the JWKS every [`JWKS_REFRESH_INTERVAL`] until the issuer
    /// is dropped. A failed refresh is logged and keeps the last keys.
    pub fn keep_fresh(self: &Arc<Self>) {
        let issuer = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(JWKS_REFRESH_INTERVAL);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                let Some(issuer) = issuer.upgrade() else {
                    return;
                };
                let mut refresh = issuer.refresh.lock().await;
                // The outcome is logged and kept in `refresh`; requests
                // decide with it.
                let _ = issuer.fetch_into(&mut refresh, unix_now()).await;
            }
        });
    }

    /// Verifies `jwt` at `now` (Unix seconds) and maps its claims to a user.
    ///
    /// # Errors
    ///
    /// [`AuthError::Invalid`] for a token that doesn't verify (see the
    /// module documentation), [`AuthError::Expired`] at or past its `exp`,
    /// and [`AuthError::Unavailable`] when the keys that could verify it
    /// can't be fetched and the cached ones are too old or lack its `kid`.
    pub async fn verify(&self, jwt: &str, now: u64) -> Result<UserPrincipal, AuthError> {
        if jwt.len() > MAX_TOKEN_BYTES {
            return Err(AuthError::Invalid);
        }
        let header = jsonwebtoken::decode_header(jwt).map_err(|_| AuthError::Invalid)?;
        if !ACCEPTED.contains(&header.alg) || header.crit.is_some() {
            return Err(AuthError::Invalid);
        }
        let kid = header.kid.as_deref().ok_or(AuthError::Invalid)?;
        let key = self.key(kid, now).await?;
        if key.alg != header.alg {
            return Err(AuthError::Invalid);
        }
        let mut validation = Validation::new(key.alg);
        // Time, issuer and audience are checked below against `now` and
        // this issuer's settings, not the library's clock.
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        validation.validate_nbf = false;
        validation.validate_aud = false;
        let claims = jsonwebtoken::decode::<Map<String, Value>>(jwt, &key.key, &validation)
            .map_err(|_| AuthError::Invalid)?
            .claims;
        self.principal(&claims, now)
    }

    /// The key `kid` names, fetching the JWKS once if the cache lacks it or
    /// is stale, at most once per [`UNKNOWN_KID_REFRESH_INTERVAL`].
    async fn key(&self, kid: &str, now: u64) -> Result<IssuerKey, AuthError> {
        let cached = self.jwks.load();
        if !self.stale(&cached, now)
            && let Some(key) = cached.keys.get(kid)
        {
            return Ok(key.clone());
        }
        drop(cached);
        let mut refresh = self.refresh.lock().await;
        // Another request may have fetched while this one waited.
        let cached = self.jwks.load_full();
        let stale = self.stale(&cached, now);
        if !stale && let Some(key) = cached.keys.get(kid) {
            return Ok(key.clone());
        }
        let next = refresh
            .last_attempt
            .saturating_add(UNKNOWN_KID_REFRESH_INTERVAL.as_secs());
        if now < next {
            return Err(if stale || refresh.failed {
                AuthError::Unavailable
            } else {
                AuthError::Invalid
            });
        }
        let jwks = self.fetch_into(&mut refresh, now).await?;
        jwks.keys.get(kid).cloned().ok_or(AuthError::Invalid)
    }

    /// Fetches the JWKS and caches it, recording the attempt in `refresh`.
    /// A failure is logged with the issuer's name and keeps the last keys.
    async fn fetch_into(&self, refresh: &mut Refresh, now: u64) -> Result<Arc<Jwks>, AuthError> {
        refresh.last_attempt = now;
        match fetch_keys(&self.http, &self.jwks_uri).await {
            Ok(keys) => {
                refresh.failed = false;
                let jwks = Arc::new(Jwks {
                    keys,
                    fetched_at: now,
                });
                self.jwks.store(Arc::clone(&jwks));
                Ok(jwks)
            }
            Err(reason) => {
                refresh.failed = true;
                tracing::warn!(
                    issuer = %self.cfg.name,
                    "could not refresh the OIDC issuer's JWKS, keeping the last keys: {reason}"
                );
                Err(AuthError::Unavailable)
            }
        }
    }

    fn stale(&self, jwks: &Jwks, now: u64) -> bool {
        now.saturating_sub(jwks.fetched_at) > self.cfg.jwks_max_stale.as_secs()
    }

    /// The user `claims` name, after the issuer, audience and time checks.
    fn principal(&self, claims: &Map<String, Value>, now: u64) -> Result<UserPrincipal, AuthError> {
        if claims.get("iss").and_then(Value::as_str) != Some(self.cfg.issuer.as_str()) {
            return Err(AuthError::Invalid);
        }
        let audience_ok = match claims.get("aud") {
            Some(Value::String(aud)) => self.cfg.audiences.contains(aud),
            Some(Value::Array(auds)) => auds
                .iter()
                .filter_map(Value::as_str)
                .any(|aud| self.cfg.audiences.iter().any(|a| a == aud)),
            _ => false,
        };
        if !audience_ok {
            return Err(AuthError::Invalid);
        }
        let exp = claims
            .get("exp")
            .and_then(Value::as_u64)
            .ok_or(AuthError::Invalid)?;
        if now >= exp {
            return Err(AuthError::Expired);
        }
        let latest = now.saturating_add(MAX_CLOCK_SKEW.as_secs());
        for name in ["nbf", "iat"] {
            if let Some(value) = claims.get(name)
                && value.as_u64().is_none_or(|at| at > latest)
            {
                return Err(AuthError::Invalid);
            }
        }
        let sub = claims
            .get(&self.cfg.subject_claim)
            .and_then(Value::as_str)
            .ok_or(AuthError::Invalid)?;
        let subject = format!("{}:{sub}", self.cfg.name);
        crate::check_subject(sub)
            .and_then(|()| crate::check_subject(&subject))
            .map_err(|_| AuthError::Invalid)?;
        let text = |claim: &str| {
            claims
                .get(claim)
                .and_then(Value::as_str)
                .filter(|s| clean(s))
                .map(str::to_owned)
        };
        let mut mapped = BTreeMap::new();
        for claim in &self.cfg.claims {
            let values: Vec<String> = match claims.get(claim) {
                Some(Value::String(value)) => vec![value.clone()],
                Some(Value::Array(values)) => values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            };
            // A value that could forge a log line is dropped: rules only
            // grant, so a dropped value can only narrow access.
            let values: Vec<String> = values.into_iter().filter(|v| clean(v)).collect();
            if !values.is_empty() {
                mapped.insert(claim.clone(), values);
            }
        }
        Ok(UserPrincipal {
            name: text(&self.cfg.name_claim).unwrap_or_else(|| subject.clone()),
            email: text(&self.cfg.email_claim),
            subject,
            claims: mapped,
        })
    }
}

/// Whether `text` is non-empty and free of the characters that could forge
/// or disguise a log line.
fn clean(text: &str) -> bool {
    !text.trim().is_empty() && !text.chars().any(crate::is_disallowed)
}

/// The `iss` claim of `jwt`'s payload, unverified: it only picks the issuer
/// that then verifies the token.
pub(crate) fn unverified_issuer(jwt: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Issuer {
        iss: String,
    }
    if jwt.len() > MAX_TOKEN_BYTES {
        return None;
    }
    let payload = jwt.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    serde_json::from_slice::<Issuer>(&bytes)
        .ok()
        .map(|issuer| issuer.iss)
}

/// The keys of the JWKS at `url` a token may be verified with, by `kid`.
async fn fetch_keys(
    http: &reqwest::Client,
    url: &Url,
) -> Result<BTreeMap<String, IssuerKey>, String> {
    #[derive(Deserialize)]
    struct JwkSet {
        keys: Vec<Value>,
    }
    let body = fetch(http, url).await?;
    let set: JwkSet = serde_json::from_slice(&body)
        .map_err(|err| format!("the JWKS at {url} is not a JSON Web Key Set: {err}"))?;
    let mut keys = BTreeMap::new();
    for value in set.keys {
        // A key of a type or use OpenXet doesn't verify with is skipped,
        // as RFC 7517 §5 asks of unknown members.
        let Ok(jwk) = serde_json::from_value::<Jwk>(value) else {
            continue;
        };
        if let Some((kid, key)) = issuer_key(&jwk) {
            keys.entry(kid).or_insert(key);
        }
    }
    Ok(keys)
}

/// The `kid` and verifying key of `jwk`, if a token may be verified with
/// it: a signing key with a `kid`, whose `alg`, stated or implied by its
/// type and curve, is one of [`ACCEPTED`]. An RSA key without `alg` is
/// RS256, the algorithm OIDC Core §3.1.3.7 defaults ID tokens to.
fn issuer_key(jwk: &Jwk) -> Option<(String, IssuerKey)> {
    let kid = jwk.common.key_id.clone()?;
    if jwk
        .common
        .public_key_use
        .as_ref()
        .is_some_and(|u| *u != PublicKeyUse::Signature)
    {
        return None;
    }
    let stated = jwk.common.key_algorithm;
    let alg = match &jwk.algorithm {
        AlgorithmParameters::RSA(_) => match stated {
            None | Some(KeyAlgorithm::RS256) => Algorithm::RS256,
            Some(KeyAlgorithm::PS256) => Algorithm::PS256,
            _ => return None,
        },
        AlgorithmParameters::EllipticCurve(ec) => match (&ec.curve, stated) {
            (EllipticCurve::P256, None | Some(KeyAlgorithm::ES256)) => Algorithm::ES256,
            (EllipticCurve::P384, None | Some(KeyAlgorithm::ES384)) => Algorithm::ES384,
            _ => return None,
        },
        AlgorithmParameters::OctetKeyPair(okp) => match (&okp.curve, stated) {
            (EllipticCurve::Ed25519, None | Some(KeyAlgorithm::EdDSA)) => Algorithm::EdDSA,
            _ => return None,
        },
        _ => return None,
    };
    let key = DecodingKey::from_jwk(jwk).ok()?;
    Some((kid, IssuerKey { alg, key }))
}

/// The body at `url`, of a `2xx` answer and at most
/// [`MAX_DOCUMENT_BYTES`].
async fn fetch(http: &reqwest::Client, url: &Url) -> Result<Vec<u8>, String> {
    let mut response = http
        .get(url.clone())
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|err| format!("could not fetch {url}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{url} answered {}", response.status()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| format!("could not read {url}: {err}"))?
    {
        if body.len() + chunk.len() > MAX_DOCUMENT_BYTES {
            return Err(format!("{url} answered more than 1 MiB"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn names_are_short_identifiers_and_never_reserved() {
        for good in ["corp", "github-actions", "a.b_c", "0"] {
            assert_eq!(check_name(good), Ok(()), "{good}");
        }
        for bad in [
            "",
            "a:b",
            "-a",
            "with space",
            "delegated",
            "anonymous",
            &"a".repeat(65),
        ] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn endpoints_are_https_or_loopback_http() {
        for good in [
            "https://id.example.com",
            "http://127.0.0.1:8080",
            "http://[::1]/realms/x",
            "http://localhost:9000",
        ] {
            assert_eq!(check_endpoint(&Url::parse(good).unwrap()), Ok(()), "{good}");
        }
        for bad in [
            "http://id.example.com",
            "ftp://127.0.0.1",
            "file:///etc/jwks",
        ] {
            assert!(check_endpoint(&Url::parse(bad).unwrap()).is_err(), "{bad}");
        }
    }
}
