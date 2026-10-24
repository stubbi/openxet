//! CAS-token authentication of untrusted bearer bytes (ADR 0007; rulings
//! M1-R18, M1-R19). Input byte 0 picks the mode:
//!
//! - even: the rest is the bearer as sent. It never panics. A CAS token
//!   can't pass without a signature from the trusted key, so the only
//!   principal raw bytes can yield is the configured static token's, for
//!   exactly that token (seeded by fuzz/seed.sh).
//! - odd: the rest describes claims, a token for them is minted with the
//!   trusted key, and bytes of it are flipped. Unflipped, it authenticates
//!   as exactly the minted principal; flipped, it either fails or still
//!   yields exactly that principal, never another one.

#![no_main]

use std::sync::OnceLock;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use libfuzzer_sys::arbitrary::{Result as ArbitraryResult, Unstructured};
use libfuzzer_sys::fuzz_target;
use openxet_auth::{Authenticator, MAX_SCOPE_BYTES, Mode, Principal, StaticToken, TokenSigner};

/// A fixed `now`, so a minted token's validity never depends on the clock.
const NOW: u64 = 1_800_000_000;

/// The PKCS#8 (RFC 8410) DER prefix of an Ed25519 private key; the 32-byte
/// seed follows. The key is built here rather than committed as a PEM.
const PKCS8_ED25519_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

fn keys() -> &'static (TokenSigner, Authenticator) {
    static KEYS: OnceLock<(TokenSigner, Authenticator)> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut der = PKCS8_ED25519_PREFIX.to_vec();
        der.extend_from_slice(&[7; 32]);
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n",
            STANDARD.encode(der)
        );
        let signer = TokenSigner::from_pem(&pem).expect("a valid Ed25519 key");
        let authenticator = Authenticator::new(
            vec![static_token()],
            vec![signer.public_key()],
            Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
        )
        .expect("valid settings");
        (signer, authenticator)
    })
}

fn assert_valid_identity(principal: &Principal) {
    for value in [&principal.subject, &principal.scope] {
        assert!(!value.trim().is_empty(), "{principal:?}");
        assert!(!value.chars().any(char::is_control), "{principal:?}");
    }
    assert!(principal.scope.len() <= MAX_SCOPE_BYTES, "{principal:?}");
}

/// The one static token configured: `oxt_` and 43 `A`s, which is 32 zero
/// bytes in base64url. Built here, so no token-shaped string is committed.
fn static_secret() -> String {
    format!("oxt_{}", "A".repeat(43))
}

fn static_token() -> StaticToken {
    StaticToken {
        sha256: StaticToken::digest(&static_secret()),
        scope: "model/org/repo".to_owned(),
        mode: Mode::Read,
        subject: "fuzz".to_owned(),
    }
}

fn raw(bearer: &[u8]) {
    let Ok(bearer) = std::str::from_utf8(bearer) else {
        return;
    };
    if let Ok(principal) = keys().1.authenticate(bearer, NOW) {
        assert_eq!(bearer, static_secret(), "{principal:?}");
        assert_valid_identity(&principal);
    }
}

fn minted(input: &mut Unstructured<'_>) -> ArbitraryResult<()> {
    let subject: String = input.arbitrary()?;
    let scope: String = input.arbitrary()?;
    let mode = if input.arbitrary()? {
        Mode::Write
    } else {
        Mode::Read
    };
    let max = Authenticator::DEFAULT_MAX_TOKEN_LIFETIME.as_secs();
    let lifetime = Duration::from_secs(input.int_in_range(1..=max)?);
    let flips: Vec<(u16, u8)> = input.arbitrary()?;
    let (signer, authenticator) = keys();
    let Ok(token) = signer.sign(&subject, &scope, mode, NOW + lifetime.as_secs(), NOW) else {
        return Ok(());
    };
    let expected = Principal {
        subject,
        scope,
        mode,
    };
    let mut bytes = token.into_bytes();
    let len = bytes.len();
    for (at, mask) in &flips {
        bytes[usize::from(*at) % len] ^= mask;
    }
    let Ok(bearer) = String::from_utf8(bytes) else {
        return Ok(());
    };
    match authenticator.authenticate(&bearer, NOW) {
        Ok(principal) => assert_eq!(principal, expected),
        Err(err) => assert!(
            flips.iter().any(|&(_, mask)| mask != 0),
            "an unaltered minted token was refused: {err}"
        ),
    }
    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    if mode % 2 == 0 {
        raw(rest);
    } else {
        let _ = minted(&mut Unstructured::new(rest));
    }
});
