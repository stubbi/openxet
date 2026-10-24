//! CAS tokens: compact JWS (RFC 7515 §7.1) with `alg: EdDSA` over Ed25519
//! (RFC 8037), ADR 0007, ruling M1-R18.
//!
//! Signing and verification both sit directly on `ed25519-dalek`; this
//! module only adds base64url and JSON around it. Verification accepts one
//! shape and nothing else:
//!
//! - at most [`MAX_TOKEN_BYTES`], checked before any decoding;
//! - exactly three segments, each canonical unpadded base64url;
//! - a header that is exactly `{"alg":"EdDSA","typ":"JWT","kid":"…"}`, so
//!   key-carrying or critical parameters (`jku`, `jwk`, `x5u`, `crit`, ...)
//!   can never take effect;
//! - an Ed25519 signature over `header_b64.payload_b64` as received,
//!   checked with `verify_strict` against the key named by `kid`;
//! - claims `sub`, `scope`, `mode` (`"read"` or `"write"`), `exp` and `iat`,
//!   plus optional `nbf`, and nothing else; `exp` is exclusive and `nbf`
//!   inclusive, with no leeway; `exp` at most the lifetime cap after `now`
//!   and `iat` at most [`crate::MAX_CLOCK_SKEW`] ahead of it; `sub` and `scope` pass
//!   [`check_identity`](crate::check_identity).

use std::fmt::{self, Write as _};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::Signature;
use ed25519_dalek::pkcs8::{
    DecodePrivateKey as _, DecodePublicKey as _, EncodePrivateKey as _, EncodePublicKey as _,
    KeypairBytes, spki::der::pem::LineEnding,
};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

use crate::{AuthError, Mode, Principal};

/// The longest accepted CAS token, in bytes. Real tokens are a few hundred
/// bytes; the cap bounds decoding work for hostile input.
pub const MAX_TOKEN_BYTES: usize = 8 * 1024;

/// An Ed25519 public key trusted to sign CAS tokens.
#[derive(Clone)]
pub struct PublicKey {
    kid: String,
    key: VerifyingKey,
}

impl PublicKey {
    /// Loads a PEM-encoded `SubjectPublicKeyInfo` (`-----BEGIN PUBLIC
    /// KEY-----`, RFC 8410).
    ///
    /// # Errors
    ///
    /// [`AuthError::InvalidKey`] if `pem` is not an Ed25519 public key, or is
    /// a weak (small-order) one.
    pub fn from_pem(pem: &str) -> Result<Self, AuthError> {
        let key = VerifyingKey::from_public_key_pem(pem).map_err(|_| AuthError::InvalidKey {
            reason: "expected a PEM-encoded Ed25519 public key (SubjectPublicKeyInfo, `BEGIN PUBLIC KEY`)",
        })?;
        if key.is_weak() {
            return Err(AuthError::InvalidKey {
                reason: "the Ed25519 public key has small order and would accept forged signatures",
            });
        }
        Ok(Self::from_verifying_key(&key))
    }

    /// The key id: the first 16 lowercase hex characters of the SHA-256 of
    /// the raw 32-byte public key. Tokens name their key with it in the
    /// `kid` header.
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    fn from_verifying_key(key: &VerifyingKey) -> Self {
        Self {
            kid: kid_of(key),
            key: *key,
        }
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

/// Signs CAS tokens with an Ed25519 private key.
pub struct TokenSigner {
    kid: String,
    key: SigningKey,
    max_token_lifetime: Duration,
}

impl TokenSigner {
    /// Loads a PEM-encoded PKCS#8 private key (`-----BEGIN PRIVATE
    /// KEY-----`, RFC 8410), as written by [`generate_keypair`].
    ///
    /// # Errors
    ///
    /// [`AuthError::InvalidKey`] if `pem` is not an Ed25519 private key.
    pub fn from_pem(pem: &str) -> Result<Self, AuthError> {
        let key = SigningKey::from_pkcs8_pem(pem).map_err(|_| AuthError::InvalidKey {
            reason: "expected a PEM-encoded Ed25519 private key (PKCS#8, `BEGIN PRIVATE KEY`)",
        })?;
        Ok(Self {
            kid: kid_of(&key.verifying_key()),
            key,
            max_token_lifetime: crate::Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
        })
    }

    /// The id of this signer's key; see [`PublicKey::kid`].
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The public key that verifies this signer's tokens.
    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        PublicKey::from_verifying_key(&self.key.verifying_key())
    }

    /// Limits the lifetime of tokens this signer mints; it should match the
    /// verifiers' `max_token_lifetime`. The default is
    /// [`Authenticator::DEFAULT_MAX_TOKEN_LIFETIME`](crate::Authenticator::DEFAULT_MAX_TOKEN_LIFETIME).
    ///
    /// # Errors
    ///
    /// [`AuthError::LifetimeOutOfRange`] outside
    /// [`Authenticator::MIN_TOKEN_LIFETIME`](crate::Authenticator::MIN_TOKEN_LIFETIME)
    /// to [`Authenticator::MAX_TOKEN_LIFETIME`](crate::Authenticator::MAX_TOKEN_LIFETIME).
    pub fn with_max_token_lifetime(
        mut self,
        max_token_lifetime: Duration,
    ) -> Result<Self, AuthError> {
        crate::check_token_lifetime(max_token_lifetime)?;
        self.max_token_lifetime = max_token_lifetime;
        Ok(self)
    }

    /// A CAS token for `subject` limited to `scope` and `mode`, issued at
    /// `now` and valid until `exp` (Unix seconds, exclusive).
    ///
    /// # Errors
    ///
    /// [`AuthError::InvalidClaims`] for a token the verifier would reject:
    /// an empty or control-character `subject` or `scope`, a `scope` over
    /// [`MAX_SCOPE_BYTES`](crate::MAX_SCOPE_BYTES), `exp <= now`, or `exp`
    /// more than this signer's lifetime limit after `now`.
    pub fn sign(
        &self,
        subject: &str,
        scope: &str,
        mode: Mode,
        exp: u64,
        now: u64,
    ) -> Result<String, AuthError> {
        crate::check_identity(subject, scope)
            .map_err(|reason| AuthError::InvalidClaims { reason })?;
        if exp <= now {
            return Err(AuthError::InvalidClaims {
                reason: "exp is not after now, so the token would already be expired",
            });
        }
        if exp > now.saturating_add(self.max_token_lifetime.as_secs()) {
            return Err(AuthError::InvalidClaims {
                reason: "exp is further ahead than the maximum token lifetime",
            });
        }
        let header = serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": self.kid });
        let claims = serde_json::json!({
            "sub": subject, "scope": scope, "mode": mode, "exp": exp, "iat": now,
        });
        let mut token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string()),
        );
        let signature = self.key.sign(token.as_bytes());
        token.push('.');
        token.push_str(&URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        Ok(token)
    }
}

impl fmt::Debug for TokenSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSigner")
            .field("kid", &self.kid)
            .field("key", &"<redacted>")
            .field("max_token_lifetime", &self.max_token_lifetime)
            .finish()
    }
}

/// A freshly generated Ed25519 key pair, PEM-encoded.
pub struct GeneratedKeypair {
    private_pem: Zeroizing<String>,
    public_pem: String,
    kid: String,
}

impl GeneratedKeypair {
    /// The private key as PKCS#8 PEM, for [`TokenSigner::from_pem`].
    #[must_use]
    pub fn private_pem(&self) -> &str {
        &self.private_pem
    }

    /// The public key as `SubjectPublicKeyInfo` PEM, for
    /// [`PublicKey::from_pem`].
    #[must_use]
    pub fn public_pem(&self) -> &str {
        &self.public_pem
    }

    /// The key id; see [`PublicKey::kid`].
    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }
}

impl fmt::Debug for GeneratedKeypair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeneratedKeypair")
            .field("kid", &self.kid)
            .field("private_pem", &"<redacted>")
            .field("public_pem", &self.public_pem)
            .finish()
    }
}

/// Generates an Ed25519 key pair from the operating system's random number
/// generator.
///
/// # Errors
///
/// [`AuthError::KeyGeneration`] if the random number generator or PEM
/// encoding fails.
pub fn generate_keypair() -> Result<GeneratedKeypair, AuthError> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|_| AuthError::KeyGeneration)?;
    let public = SigningKey::from_bytes(&seed).verifying_key();
    // PKCS#8 v1 (RFC 8410 §7), without the optional public key: Python's
    // `cryptography`, under PyJWT, refuses the v2 form `SigningKey` writes.
    let mut private = KeypairBytes {
        secret_key: seed,
        public_key: None,
    };
    seed.zeroize();
    // `KeypairBytes` holds its own copy of the seed and wipes it on drop only
    // under the `ed25519` crate's `zeroize` feature, which nothing enables.
    let private_pem = private.to_pkcs8_pem(LineEnding::LF);
    private.secret_key.zeroize();
    Ok(GeneratedKeypair {
        private_pem: private_pem.map_err(|_| AuthError::KeyGeneration)?,
        public_pem: public
            .to_public_key_pem(LineEnding::LF)
            .map_err(|_| AuthError::KeyGeneration)?,
        kid: kid_of(&public),
    })
}

/// The only header openxet accepts.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    alg: String,
    typ: String,
    kid: String,
}

/// The claims openxet accepts (ADR 0007, ruling M1-R19). Anything else,
/// such as `aud`, `iss` or `jti`, is rejected rather than ignored: openxet
/// cannot honour an audience or issuer restriction, so a token that asks for
/// one must not be accepted.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    sub: String,
    scope: String,
    mode: Mode,
    exp: u64,
    iat: u64,
    #[serde(default, deserialize_with = "present_u64")]
    nbf: Option<u64>,
}

/// An optional claim that, when present, must be a `u64` (not `null`).
fn present_u64<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
    u64::deserialize(d).map(Some)
}

pub(crate) fn verify(
    keys: &[PublicKey],
    token: &str,
    now: u64,
    max_lifetime: Duration,
) -> Result<Principal, AuthError> {
    if token.len() > MAX_TOKEN_BYTES {
        return Err(AuthError::Invalid);
    }
    let mut segments = token.split('.');
    let (Some(header_b64), Some(payload_b64), Some(signature_b64), None) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return Err(AuthError::Invalid);
    };

    let header: Header = decode_json(header_b64)?;
    if header.alg != "EdDSA" || header.typ != "JWT" {
        return Err(AuthError::Invalid);
    }
    let key = keys
        .iter()
        .find(|k| k.kid == header.kid)
        .ok_or(AuthError::Invalid)?;
    verify_signature(&key.key, header_b64, payload_b64, signature_b64)?;

    let claims: Claims = decode_json(payload_b64)?;
    crate::check_identity(&claims.sub, &claims.scope).map_err(|_| AuthError::Invalid)?;
    if claims.nbf.is_some_and(|nbf| now < nbf) {
        return Err(AuthError::Invalid);
    }
    if now >= claims.exp {
        return Err(AuthError::Expired);
    }
    if claims.exp > now.saturating_add(max_lifetime.as_secs())
        || claims.iat > now.saturating_add(crate::MAX_CLOCK_SKEW.as_secs())
    {
        return Err(AuthError::Invalid);
    }
    Ok(Principal {
        subject: claims.sub,
        scope: claims.scope,
        mode: claims.mode,
    })
}

/// Checks the Ed25519 signature of a compact JWS, independent of header
/// policy (RFC 8037 §3.1).
fn verify_signature(
    key: &VerifyingKey,
    header_b64: &str,
    payload_b64: &str,
    signature_b64: &str,
) -> Result<(), AuthError> {
    let signature: [u8; 64] = decode_b64(signature_b64)?
        .try_into()
        .map_err(|_| AuthError::Invalid)?;
    let mut input = Vec::with_capacity(header_b64.len() + 1 + payload_b64.len());
    input.extend_from_slice(header_b64.as_bytes());
    input.push(b'.');
    input.extend_from_slice(payload_b64.as_bytes());
    key.verify_strict(&input, &Signature::from_bytes(&signature))
        .map_err(|_| AuthError::Invalid)
}

/// Decodes canonical unpadded base64url: the engine rejects padding, other
/// alphabets and non-zero trailing bits.
fn decode_b64(segment: &str) -> Result<Vec<u8>, AuthError> {
    URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| AuthError::Invalid)
}

fn decode_json<T: serde::de::DeserializeOwned>(segment: &str) -> Result<T, AuthError> {
    serde_json::from_slice(&decode_b64(segment)?).map_err(|_| AuthError::Invalid)
}

fn kid_of(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut kid = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(kid, "{byte:02x}");
    }
    kid
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Authenticator;

    // RFC 8037 Appendix A: the A.1 key pair and the A.4 example JWS, whose
    // header `{"alg":"EdDSA"}` deliberately lacks openxet's `typ`/`kid`, so
    // this exercises the signature path alone.
    const A1_PRIVATE_D: &str = "nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A";
    const A1_PUBLIC_X: &str = "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo";
    const A4_HEADER: &str = "eyJhbGciOiJFZERTQSJ9";
    const A4_PAYLOAD: &str = "RXhhbXBsZSBvZiBFZDI1NTE5IHNpZ25pbmc";
    const A4_SIGNATURE: &str =
        "hgyY0il_MGCjP0JzlnLWG1PPOt7-09PGcvMg3AIbQR6dWbhijcNR4ki4iylGjg5BhVsPt9g7sVvpAr_MuM0KAg";

    fn a1_public() -> VerifyingKey {
        let x: [u8; 32] = decode_b64(A1_PUBLIC_X)
            .expect("base64url")
            .try_into()
            .expect("32 bytes");
        VerifyingKey::from_bytes(&x).expect("valid point")
    }

    #[test]
    fn rfc8037_a4_vector_verifies() {
        assert_eq!(decode_b64(A4_HEADER).expect("b64"), br#"{"alg":"EdDSA"}"#);
        assert_eq!(
            decode_b64(A4_PAYLOAD).expect("b64"),
            b"Example of Ed25519 signing"
        );
        assert_eq!(
            verify_signature(&a1_public(), A4_HEADER, A4_PAYLOAD, A4_SIGNATURE),
            Ok(())
        );

        // Ed25519 is deterministic: the A.1 private key reproduces A.4.
        let d: [u8; 32] = decode_b64(A1_PRIVATE_D)
            .expect("base64url")
            .try_into()
            .expect("32 bytes");
        let signing = SigningKey::from_bytes(&d);
        assert_eq!(signing.verifying_key(), a1_public());
        let signature = signing.sign(format!("{A4_HEADER}.{A4_PAYLOAD}").as_bytes());
        assert_eq!(URL_SAFE_NO_PAD.encode(signature.to_bytes()), A4_SIGNATURE);

        let other_payload = URL_SAFE_NO_PAD.encode("Example of Ed25519 signing!");
        assert_eq!(
            verify_signature(&a1_public(), A4_HEADER, &other_payload, A4_SIGNATURE),
            Err(AuthError::Invalid)
        );

        // The full token fails only on openxet's header policy.
        let key = PublicKey::from_verifying_key(&a1_public());
        let token = format!("{A4_HEADER}.{A4_PAYLOAD}.{A4_SIGNATURE}");
        assert_eq!(
            verify(&[key], &token, 0, Authenticator::DEFAULT_MAX_TOKEN_LIFETIME),
            Err(AuthError::Invalid)
        );
    }

    /// A signature with `R` = the identity point and `S = k·a`, which only
    /// the key holder can make. Cofactorless non-strict verification accepts
    /// it, giving a second valid signature per message; `verify_strict`
    /// rejects small-order `R` (RFC 8032 §5.1.7 leaves this to the verifier).
    #[test]
    fn verification_is_strict() {
        use curve25519_dalek::scalar::{Scalar, clamp_integer};
        use ed25519_dalek::Verifier as _;
        use sha2::Sha512;

        let seed = [42u8; 32];
        let signing = SigningKey::from_bytes(&seed);
        let public = PublicKey::from_verifying_key(&signing.verifying_key());
        let header = URL_SAFE_NO_PAD.encode(
            serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": public.kid() }).to_string(),
        );
        let payload = URL_SAFE_NO_PAD.encode(
            serde_json::json!({
                "sub": "alice", "scope": "org/model", "mode": "write", "exp": 100, "iat": 0,
            })
            .to_string(),
        );
        let input = format!("{header}.{payload}");

        let mut identity = [0u8; 32];
        identity[0] = 1;
        let expanded: [u8; 64] = Sha512::digest(seed).into();
        let mut lower = [0u8; 32];
        lower.copy_from_slice(&expanded[..32]);
        let a = Scalar::from_bytes_mod_order(clamp_integer(lower));
        let k: [u8; 64] = Sha512::new()
            .chain_update(identity)
            .chain_update(signing.verifying_key().as_bytes())
            .chain_update(input.as_bytes())
            .finalize()
            .into();
        let s = Scalar::from_bytes_mod_order_wide(&k) * a;
        let mut weak = [0u8; 64];
        weak[..32].copy_from_slice(&identity);
        weak[32..].copy_from_slice(s.as_bytes());

        let signature = Signature::from_bytes(&weak);
        assert!(
            signing
                .verifying_key()
                .verify(input.as_bytes(), &signature)
                .is_ok(),
            "the probe must be accepted by non-strict verification"
        );
        let token = format!("{input}.{}", URL_SAFE_NO_PAD.encode(weak));
        assert_eq!(
            verify(
                &[public],
                &token,
                0,
                Authenticator::DEFAULT_MAX_TOKEN_LIFETIME
            ),
            Err(AuthError::Invalid)
        );
    }
}
