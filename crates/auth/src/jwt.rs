//! CAS tokens: compact JWS with `alg: EdDSA` over Ed25519 (RFC 8037), ADR
//! 0007.
//!
//! Tokens are verified with `jsonwebtoken`, restricted to `EdDSA`. Signing
//! builds the compact form directly from `ed25519-dalek`, which cannot fail
//! once the key is loaded, so [`TokenSigner::sign`] needs no error path.

use std::fmt::{self, Write as _};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::pkcs8::{
    DecodePrivateKey as _, DecodePublicKey as _, EncodePrivateKey as _, EncodePublicKey as _,
    spki::der::pem::LineEnding,
};
use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

use crate::{AuthError, Mode, Principal};

/// An Ed25519 public key trusted to sign CAS tokens.
#[derive(Clone)]
pub struct PublicKey {
    kid: String,
    decoding: DecodingKey,
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
            decoding: DecodingKey::from_ed_der(key.as_bytes()),
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

    /// A CAS token for `subject` limited to `scope` and `mode`, valid until
    /// `exp` (Unix seconds, exclusive). `iat` is the current time.
    #[must_use]
    pub fn sign(&self, subject: &str, scope: &str, mode: Mode, exp: u64) -> String {
        let iat = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let header = serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": self.kid });
        let claims = serde_json::json!({
            "sub": subject, "scope": scope, "mode": mode, "exp": exp, "iat": iat,
        });
        let mut token = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string()),
        );
        let signature = self.key.sign(token.as_bytes());
        token.push('.');
        token.push_str(&URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        token
    }
}

impl fmt::Debug for TokenSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSigner")
            .field("kid", &self.kid)
            .field("key", &"<redacted>")
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
    let key = SigningKey::from_bytes(&seed);
    seed.zeroize();
    let public = key.verifying_key();
    Ok(GeneratedKeypair {
        private_pem: key
            .to_pkcs8_pem(LineEnding::LF)
            .map_err(|_| AuthError::KeyGeneration)?,
        public_pem: public
            .to_public_key_pem(LineEnding::LF)
            .map_err(|_| AuthError::KeyGeneration)?,
        kid: kid_of(&public),
    })
}

/// The claims openxet reads; others (`iat`, `nbf`, `aud`, ...) are ignored.
#[derive(Deserialize)]
struct Claims {
    sub: String,
    scope: String,
    mode: Mode,
    exp: u64,
}

pub(crate) fn verify(keys: &[PublicKey], token: &str, now: u64) -> Result<Principal, AuthError> {
    let header = jsonwebtoken::decode_header(token).map_err(|_| AuthError::Invalid)?;
    if header.alg != Algorithm::EdDSA {
        return Err(AuthError::Invalid);
    }
    let kid = header.kid.ok_or(AuthError::Invalid)?;
    let key = keys
        .iter()
        .find(|k| k.kid == kid)
        .ok_or(AuthError::Invalid)?;

    // `exp` is checked below against the caller's clock, with no leeway.
    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = false;
    validation.leeway = 0;
    validation.validate_aud = false;
    let claims = jsonwebtoken::decode::<Claims>(token, &key.decoding, &validation)
        .map_err(|_| AuthError::Invalid)?
        .claims;

    if now >= claims.exp {
        return Err(AuthError::Expired);
    }
    Ok(Principal {
        subject: claims.sub,
        scope: claims.scope,
        mode: claims.mode,
    })
}

fn kid_of(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut kid = String::with_capacity(16);
    for byte in &digest[..8] {
        let _ = write!(kid, "{byte:02x}");
    }
    kid
}
