//! Behaviour of `openxet-auth`: static tokens, `EdDSA` CAS tokens and
//! range-bound signed transfer URLs (design §5.4, §5.5, §5.9; ADR 0007;
//! rulings M1-R2, M1-R3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::pkcs8::{DecodePrivateKey as _, DecodePublicKey as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};
use hmac::{Hmac, KeyInit as _, Mac as _};
use openxet_auth::{
    AuthError, Authenticator, ForbiddenReason, MAX_TOKEN_BYTES, Mode, Principal, PublicKey,
    StaticToken, TokenSigner, TransferQuery, UrlSigner, generate_keypair,
};
use openxet_protocol::{MerkleHash, chunk_hash, hash_to_hex};
use sha2::Sha256;
use url::Url;

const NOW: u64 = 1_800_000_000;
const ALPHABET: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const STATIC_SECRET: &str = "oxt_static_secret_never_printed_0123456789";

fn static_token(secret: &str, mode: Mode) -> StaticToken {
    StaticToken {
        sha256: StaticToken::digest(secret),
        scope: "org/model".to_owned(),
        mode,
        subject: "ci".to_owned(),
    }
}

fn signer() -> TokenSigner {
    let pair = generate_keypair().expect("generate keypair");
    TokenSigner::from_pem(pair.private_pem()).expect("parse private key")
}

fn xorb(byte: u8) -> MerkleHash {
    chunk_hash(&[byte])
}

fn url_signer() -> UrlSigner {
    UrlSigner::new([7; 32], UrlSigner::DEFAULT_LIFETIME).expect("default lifetime is valid")
}

fn query_of(url: &Url) -> TransferQuery {
    serde_urlencoded::from_str(url.query().expect("signed URL has a query"))
        .expect("query deserializes as TransferQuery")
}

#[test]
fn static_token_matches_by_hash() {
    let auth = Authenticator::new(
        vec![static_token(STATIC_SECRET, Mode::Write)],
        vec![],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let principal = auth.authenticate(STATIC_SECRET, NOW).expect("static token");
    assert_eq!(
        principal,
        Principal {
            subject: "ci".to_owned(),
            scope: "org/model".to_owned(),
            mode: Mode::Write,
        }
    );
    assert!(principal.can_read());
    assert!(principal.can_write());
}

#[test]
fn unknown_token_is_invalid() {
    let auth = Authenticator::new(
        vec![static_token(STATIC_SECRET, Mode::Read)],
        vec![],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    assert_eq!(
        auth.authenticate("oxt_static_secret_never_printed_012345678X", NOW),
        Err(AuthError::Invalid)
    );
    assert_eq!(auth.authenticate("", NOW), Err(AuthError::Missing));
}

#[test]
fn read_mode_cannot_write() {
    let auth = Authenticator::new(
        vec![static_token(STATIC_SECRET, Mode::Read)],
        vec![],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let principal = auth.authenticate(STATIC_SECRET, NOW).expect("static token");
    assert!(principal.can_read());
    assert!(!principal.can_write());
}

#[test]
fn jwt_round_trip_and_kid_rotation() {
    let old = signer();
    let new = signer();
    let stranger = signer();
    assert_ne!(old.kid(), new.kid());
    let auth = Authenticator::new(
        vec![],
        vec![old.public_key(), new.public_key()],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");

    for s in [&old, &new] {
        let token = s.sign("alice", "org/model", Mode::Read, NOW + 60, NOW);
        let principal = auth
            .authenticate(&token, NOW)
            .expect("token from a known key");
        assert_eq!(principal.subject, "alice");
        assert_eq!(principal.scope, "org/model");
        assert_eq!(principal.mode, Mode::Read);
    }

    let token = stranger.sign("alice", "org/model", Mode::Write, NOW + 60, NOW);
    assert_eq!(auth.authenticate(&token, NOW), Err(AuthError::Invalid));
}

#[test]
fn jwt_signed_with_a_known_kid_but_another_key_is_invalid() {
    let known = signer();
    let impostor = signer();
    let auth = Authenticator::new(
        vec![],
        vec![known.public_key()],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let token = impostor.sign("alice", "org/model", Mode::Write, NOW + 60, NOW);
    let forged = retarget_kid(&token, known.kid());
    assert_eq!(auth.authenticate(&forged, NOW), Err(AuthError::Invalid));
}

#[test]
fn signer_token_decodes_with_an_independent_decoder() {
    // Decodes the signer's output without the `base64` crate, so a shared
    // encoder bug cannot hide a format error (RFC 7515 §7.1, RFC 8037 §3.1).
    let pair = generate_keypair().expect("generate keypair");
    let s = TokenSigner::from_pem(pair.private_pem()).expect("parse");
    let token = s.sign("alice", "org/model", Mode::Write, NOW + 60, NOW);
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(segments.len(), 3);

    let header: serde_json::Value =
        serde_json::from_slice(&b64url_decode_by_hand(segments[0])).expect("header JSON");
    assert_eq!(
        header,
        serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": s.kid() })
    );
    let claims: serde_json::Value =
        serde_json::from_slice(&b64url_decode_by_hand(segments[1])).expect("claims JSON");
    assert_eq!(claims["sub"], "alice");
    assert_eq!(claims["scope"], "org/model");
    assert_eq!(claims["mode"], "write");
    assert_eq!(claims["exp"], NOW + 60);
    assert_eq!(claims["iat"], NOW);

    let signature: [u8; 64] = b64url_decode_by_hand(segments[2])
        .try_into()
        .expect("64-byte Ed25519 signature");
    let public = VerifyingKey::from_public_key_pem(pair.public_pem()).expect("public key");
    public
        .verify_strict(
            format!("{}.{}", segments[0], segments[1]).as_bytes(),
            &Signature::from_bytes(&signature),
        )
        .expect("signature over header.payload verifies");
}

#[test]
fn jwt_expired_exactly_at_exp() {
    let s = signer();
    let auth = Authenticator::new(
        vec![],
        vec![s.public_key()],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let token = s.sign("alice", "org/model", Mode::Read, NOW, NOW - 60);
    assert!(auth.authenticate(&token, NOW - 1).is_ok());
    assert_eq!(auth.authenticate(&token, NOW), Err(AuthError::Expired));
    assert_eq!(auth.authenticate(&token, NOW + 1), Err(AuthError::Expired));
}

#[test]
fn jwt_wrong_alg_rejected() {
    let (key, public_pem, _) = raw_keypair();
    let kid = PublicKey::from_pem(&public_pem)
        .expect("parse")
        .kid()
        .to_owned();
    let auth = Authenticator::new(
        vec![],
        vec![PublicKey::from_pem(&public_pem).expect("parse")],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let payload = b64(&valid_claims().to_string());

    // HS256 keyed with the public key in every form a verifier might hold:
    // the classic algorithm confusion.
    let public_der = pem_body(&public_pem);
    for secret in [
        public_der.clone(),
        public_der[12..].to_vec(),
        public_pem.as_bytes().to_vec(),
    ] {
        let header =
            b64(&serde_json::json!({ "alg": "HS256", "typ": "JWT", "kid": kid }).to_string());
        let input = format!("{header}.{payload}");
        let mut mac = Hmac::<Sha256>::new_from_slice(&secret).expect("any HMAC key length");
        mac.update(input.as_bytes());
        let token = format!(
            "{input}.{}",
            URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
        );
        assert_eq!(auth.authenticate(&token, NOW), Err(AuthError::Invalid));
    }

    for alg in ["none", "None", "RS256", "ES256", "Ed25519", "eddsa"] {
        let header = b64(&serde_json::json!({ "alg": alg, "typ": "JWT", "kid": kid }).to_string());
        let signed = sign_raw(&key, &header, &payload);
        for token in [
            signed,
            format!("{header}.{payload}."),
            format!("{header}.{payload}"),
        ] {
            assert_eq!(
                auth.authenticate(&token, NOW),
                Err(AuthError::Invalid),
                "{alg}"
            );
        }
    }
}

#[test]
fn jwt_rejects_header_with_extra_fields() {
    let (key, public_pem, _) = raw_keypair();
    let kid = PublicKey::from_pem(&public_pem)
        .expect("parse")
        .kid()
        .to_owned();
    let auth = Authenticator::new(
        vec![],
        vec![PublicKey::from_pem(&public_pem).expect("parse")],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let payload = b64(&valid_claims().to_string());

    let good = serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": kid });
    assert!(
        auth.authenticate(&sign_raw(&key, &b64(&good.to_string()), &payload), NOW)
            .is_ok()
    );

    let mut headers: Vec<String> = ["jku", "jwk", "x5u", "x5c", "crit", "cty", "b64"]
        .iter()
        .map(|field| {
            let mut h = good.clone();
            h[*field] = serde_json::json!("https://attacker.example/keys");
            h.to_string()
        })
        .collect();
    headers.extend([
        serde_json::json!({ "alg": "EdDSA", "kid": kid }).to_string(),
        serde_json::json!({ "alg": "EdDSA", "typ": "JWT" }).to_string(),
        serde_json::json!({ "typ": "JWT", "kid": kid }).to_string(),
        serde_json::json!({ "alg": "EdDSA", "typ": "jwt", "kid": kid }).to_string(),
        serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": "0000000000000000" }).to_string(),
        serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": 7 }).to_string(),
        format!(r#"{{"alg":"EdDSA","typ":"JWT","kid":"{kid}","kid":"{kid}"}}"#),
        format!(r#"{{"alg":"EdDSA","alg":"none","typ":"JWT","kid":"{kid}"}}"#),
        format!(r#"[{{"alg":"EdDSA","typ":"JWT","kid":"{kid}"}}]"#),
    ]);
    for header in headers {
        let token = sign_raw(&key, &b64(&header), &payload);
        assert_eq!(
            auth.authenticate(&token, NOW),
            Err(AuthError::Invalid),
            "{header}"
        );
    }
}

#[test]
fn jwt_rejects_bad_claims() {
    let (key, public_pem, _) = raw_keypair();
    let kid = PublicKey::from_pem(&public_pem)
        .expect("parse")
        .kid()
        .to_owned();
    let auth = Authenticator::new(
        vec![],
        vec![PublicKey::from_pem(&public_pem).expect("parse")],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let header = b64(&serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": kid }).to_string());

    let mut cases = Vec::new();
    for mode in ["admin", "Write", "", "READ"] {
        let mut c = valid_claims();
        c["mode"] = serde_json::json!(mode);
        cases.push(c);
    }
    for field in ["sub", "scope", "mode", "exp", "iat"] {
        let mut c = valid_claims();
        c.as_object_mut().expect("object").remove(field);
        cases.push(c);
    }
    for exp in [
        serde_json::json!(-1),
        serde_json::json!(1.8e9),
        serde_json::json!("1800000060"),
    ] {
        let mut c = valid_claims();
        c["exp"] = exp;
        cases.push(c);
    }
    cases.push(serde_json::json!([valid_claims()]));
    for claims in cases {
        let token = sign_raw(&key, &header, &b64(&claims.to_string()));
        assert_eq!(
            auth.authenticate(&token, NOW),
            Err(AuthError::Invalid),
            "{claims}"
        );
    }
    let token = sign_raw(&key, &header, "not-json");
    assert_eq!(auth.authenticate(&token, NOW), Err(AuthError::Invalid));
}

fn auth_for_raw_tokens() -> (Authenticator, SigningKey, String) {
    let (key, public_pem, kid) = raw_keypair();
    let auth = Authenticator::new(
        vec![],
        vec![PublicKey::from_pem(&public_pem).expect("parse")],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let header = b64(&serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": kid }).to_string());
    (auth, key, header)
}

fn with_claim(field: &str, value: serde_json::Value) -> serde_json::Value {
    let mut claims = valid_claims();
    claims[field] = value;
    claims
}

#[test]
fn jwt_rejects_future_nbf() {
    let (auth, key, header) = auth_for_raw_tokens();
    for nbf in [NOW + 1, NOW + 3600, u64::MAX] {
        let token = sign_raw(
            &key,
            &header,
            &b64(&with_claim("nbf", nbf.into()).to_string()),
        );
        assert_eq!(
            auth.authenticate(&token, NOW),
            Err(AuthError::Invalid),
            "{nbf}"
        );
    }
    for nbf in [
        serde_json::json!("0"),
        serde_json::json!(-1),
        serde_json::json!(null),
    ] {
        let token = sign_raw(
            &key,
            &header,
            &b64(&with_claim("nbf", nbf.clone()).to_string()),
        );
        assert_eq!(
            auth.authenticate(&token, NOW),
            Err(AuthError::Invalid),
            "{nbf}"
        );
    }
}

#[test]
fn jwt_accepts_past_nbf() {
    let (auth, key, header) = auth_for_raw_tokens();
    for nbf in [0, NOW - 60, NOW] {
        let token = sign_raw(
            &key,
            &header,
            &b64(&with_claim("nbf", nbf.into()).to_string()),
        );
        assert!(auth.authenticate(&token, NOW).is_ok(), "{nbf}");
    }
}

#[test]
fn jwt_rejects_unknown_claims() {
    let (auth, key, header) = auth_for_raw_tokens();
    for field in ["aud", "iss", "jti", "revision", "scopes", "Sub", "pad"] {
        let token = sign_raw(
            &key,
            &header,
            &b64(&with_claim(field, "x".into()).to_string()),
        );
        assert_eq!(
            auth.authenticate(&token, NOW),
            Err(AuthError::Invalid),
            "{field}"
        );
    }
    let aud_list = with_claim("aud", serde_json::json!(["openxet"]));
    let token = sign_raw(&key, &header, &b64(&aud_list.to_string()));
    assert_eq!(auth.authenticate(&token, NOW), Err(AuthError::Invalid));
}

#[test]
fn jwt_rejects_empty_or_malformed_subject_and_scope() {
    let (auth, key, header) = auth_for_raw_tokens();
    let long_scope = "s".repeat(1025);
    let rejected = [
        ("sub", ""),
        ("sub", "alice\n"),
        ("scope", ""),
        ("scope", "org/model\n"),
        ("scope", "org/\u{0}model"),
        ("scope", "org/model\u{7f}"),
        ("scope", "org/model\u{85}"),
        ("scope", long_scope.as_str()),
    ];
    for (field, value) in rejected {
        let token = sign_raw(
            &key,
            &header,
            &b64(&with_claim(field, value.into()).to_string()),
        );
        assert_eq!(
            auth.authenticate(&token, NOW),
            Err(AuthError::Invalid),
            "{field}={value:?}"
        );
    }
    let max_scope = "s".repeat(1024);
    for (field, value) in [
        ("scope", max_scope.as_str()),
        ("scope", "org/model with space"),
        ("sub", "a"),
    ] {
        let token = sign_raw(
            &key,
            &header,
            &b64(&with_claim(field, value.into()).to_string()),
        );
        assert!(auth.authenticate(&token, NOW).is_ok(), "{field}={value:?}");
    }
}

#[test]
fn jwt_lifetime_is_capped() {
    let (key, public_pem, kid) = raw_keypair();
    let header = b64(&serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": kid }).to_string());
    let day = Authenticator::DEFAULT_MAX_TOKEN_LIFETIME.as_secs();
    assert_eq!(day, 24 * 3600);
    let token_with = |exp: u64, iat: u64| {
        let mut claims = valid_claims();
        claims["exp"] = exp.into();
        claims["iat"] = iat.into();
        sign_raw(&key, &header, &b64(&claims.to_string()))
    };
    let auth = |cap: Duration| {
        Authenticator::new(
            vec![],
            vec![PublicKey::from_pem(&public_pem).expect("parse")],
            cap,
        )
        .expect("valid config")
    };

    let default = auth(Authenticator::DEFAULT_MAX_TOKEN_LIFETIME);
    assert!(
        default
            .authenticate(&token_with(NOW + day, NOW), NOW)
            .is_ok()
    );
    for exp in [NOW + day + 1, NOW + 365 * day, u64::MAX] {
        assert_eq!(
            default.authenticate(&token_with(exp, NOW), NOW),
            Err(AuthError::Invalid),
            "{exp}"
        );
    }

    let hour = auth(Duration::from_hours(1));
    assert!(hour.authenticate(&token_with(NOW + 3600, NOW), NOW).is_ok());
    assert_eq!(
        hour.authenticate(&token_with(NOW + 3601, NOW), NOW),
        Err(AuthError::Invalid)
    );
    // The cap is measured from the verifier's clock, not the token's `iat`.
    assert!(
        hour.authenticate(&token_with(NOW + 3600, NOW + 300), NOW)
            .is_ok()
    );
}

#[test]
fn jwt_iat_tolerates_five_minutes_of_skew() {
    let (auth, key, header) = auth_for_raw_tokens();
    for (iat, ok) in [
        (0, true),
        (NOW, true),
        (NOW + 300, true),
        (NOW + 301, false),
        (u64::MAX, false),
    ] {
        let token = sign_raw(
            &key,
            &header,
            &b64(&with_claim("iat", iat.into()).to_string()),
        );
        assert_eq!(auth.authenticate(&token, NOW).is_ok(), ok, "{iat}");
    }
}

#[test]
fn static_tokens_with_invalid_subject_or_scope_are_rejected_at_construction() {
    let long_scope = "s".repeat(1025);
    for (subject, scope) in [
        ("", "org/model"),
        ("ci\n", "org/model"),
        ("ci", ""),
        ("ci", "org/model\r"),
        ("ci", long_scope.as_str()),
    ] {
        let mut token = static_token(STATIC_SECRET, Mode::Read);
        token.subject = subject.to_owned();
        token.scope = scope.to_owned();
        let good = static_token("another secret", Mode::Read);
        let error = Authenticator::new(
            vec![good, token],
            vec![],
            Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
        )
        .expect_err("invalid static token config");
        assert!(
            matches!(error, AuthError::InvalidStaticToken { index: 1, .. }),
            "{subject:?} {scope:?}: {error:?}"
        );
        assert!(!error.to_string().contains(STATIC_SECRET));
    }
}

#[test]
fn jwt_rejects_oversized_token() {
    let (key, public_pem, _) = raw_keypair();
    let kid = PublicKey::from_pem(&public_pem)
        .expect("parse")
        .kid()
        .to_owned();
    let auth = Authenticator::new(
        vec![],
        vec![PublicKey::from_pem(&public_pem).expect("parse")],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let header = b64(&serde_json::json!({ "alg": "EdDSA", "typ": "JWT", "kid": kid }).to_string());

    let sized = |len: usize| {
        let mut claims = valid_claims();
        claims["sub"] = serde_json::json!("");
        let base = sign_raw(&key, &header, &b64(&claims.to_string())).len();
        // Each 3 bytes of `sub` add 4 base64url characters.
        claims["sub"] = serde_json::json!("x".repeat((len - base) / 4 * 3));
        sign_raw(&key, &header, &b64(&claims.to_string()))
    };
    let at_cap = sized(MAX_TOKEN_BYTES);
    assert!(at_cap.len() <= MAX_TOKEN_BYTES && at_cap.len() > MAX_TOKEN_BYTES - 4);
    assert!(auth.authenticate(&at_cap, NOW).is_ok());

    let over = sized(MAX_TOKEN_BYTES + 8);
    assert!(over.len() > MAX_TOKEN_BYTES);
    assert_eq!(auth.authenticate(&over, NOW), Err(AuthError::Invalid));
    let junk = "a".repeat(1 << 20);
    assert_eq!(auth.authenticate(&junk, NOW), Err(AuthError::Invalid));
}

#[test]
fn jwt_rejects_non_canonical_base64() {
    let s = signer();
    let auth = Authenticator::new(
        vec![],
        vec![s.public_key()],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let token = s.sign("alice", "org/model", Mode::Read, NOW + 60, NOW);
    assert!(auth.authenticate(&token, NOW).is_ok());
    let (input, sig) = token.rsplit_once('.').expect("three segments");
    let (header, payload) = input.split_once('.').expect("three segments");

    // A 64-byte signature is 86 characters; the last one carries 2 unused
    // bits. Setting them re-encodes the same bytes non-canonically.
    let last = sig.chars().last().expect("non-empty");
    let index = ALPHABET.find(last).expect("base64url character");
    let sloppy = ALPHABET.as_bytes()[index | 1] as char;
    assert_ne!(sloppy, last);
    let non_canonical_sig = format!("{}{sloppy}", &sig[..sig.len() - 1]);

    let standard_alphabet = |seg: &str| seg.replace('-', "+").replace('_', "/");
    let mut variants = vec![
        format!("{input}.{non_canonical_sig}"),
        format!("{input}.{sig}=="),
        format!("{header}=.{payload}.{sig}"),
        format!("{header}.{payload}=.{sig}"),
        format!("{header} .{payload}.{sig}"),
        format!("{header}.{payload}.{sig}\n"),
        format!("{input}..{sig}"),
        format!("{input}.{sig}.{sig}"),
    ];
    if standard_alphabet(&token) != token {
        variants.push(standard_alphabet(&token));
    }
    for variant in variants {
        assert_eq!(
            auth.authenticate(&variant, NOW),
            Err(AuthError::Invalid),
            "{variant}"
        );
    }
}

#[test]
fn kid_is_stable_for_a_key() {
    let pair = generate_keypair().expect("generate keypair");
    let a = PublicKey::from_pem(pair.public_pem()).expect("parse");
    let b = PublicKey::from_pem(pair.public_pem()).expect("parse");
    let s = TokenSigner::from_pem(pair.private_pem()).expect("parse");
    assert_eq!(a.kid(), b.kid());
    assert_eq!(a.kid(), s.kid());
    assert_eq!(a.kid(), pair.kid());
    assert_eq!(a.kid().len(), 16);
    assert!(
        a.kid()
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    );
}

#[test]
fn kid_is_sha256_of_raw_public_key() {
    // RFC 8410 test key: the raw public key is the last 32 bytes of the SPKI.
    let pem = "-----BEGIN PUBLIC KEY-----\n\
               MCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n\
               -----END PUBLIC KEY-----\n";
    let key = PublicKey::from_pem(pem).expect("parse RFC 8410 key");
    let raw = &pem_body(pem)[12..];
    let digest = sha256_hex(raw);
    assert_eq!(key.kid(), &digest[..16]);
}

#[test]
fn small_order_public_keys_are_rejected_at_load() {
    // SPKI for Ed25519 (RFC 8410) wrapping points of order 1, 2 and 4.
    let spki_prefix = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];
    let mut identity = [0u8; 32];
    identity[0] = 1;
    let mut order_two = [0xffu8; 32];
    order_two[0] = 0xec;
    order_two[31] = 0x7f;
    let order_four = [0u8; 32];
    for point in [identity, order_two, order_four] {
        let der: Vec<u8> = spki_prefix.iter().chain(point.iter()).copied().collect();
        let pem = format!(
            "-----BEGIN PUBLIC KEY-----\n{}\n-----END PUBLIC KEY-----\n",
            base64::engine::general_purpose::STANDARD.encode(der)
        );
        let error = PublicKey::from_pem(&pem).expect_err("small-order key must be refused");
        assert!(
            matches!(error, AuthError::InvalidKey { .. })
                && error.to_string().contains("small order"),
            "{error}"
        );
    }
}

#[test]
fn malformed_keys_are_rejected() {
    assert!(matches!(
        PublicKey::from_pem("not a key"),
        Err(AuthError::InvalidKey { .. })
    ));
    assert!(matches!(
        TokenSigner::from_pem("not a key"),
        Err(AuthError::InvalidKey { .. })
    ));
}

#[test]
fn signed_url_has_the_documented_shape() {
    let signer = url_signer();
    let base = Url::parse("https://cas.example/prefix/").expect("url");
    let x = xorb(1);
    let url = signer.sign_xorb_range(&base, &x, 0, 99, NOW);
    assert_eq!(
        url.path(),
        format!("/prefix/v1/transfer/xorbs/{}", hash_to_hex(&x))
    );
    let q = query_of(&url);
    assert_eq!(q.range, "0-99");
    assert_eq!(q.exp, NOW + 3600);
    assert!(
        url.query()
            .expect("query")
            .starts_with(&format!("range=0-99&exp={}&sig=", NOW + 3600))
    );
}

#[test]
fn signed_url_bound_to_range() {
    let signer = url_signer();
    let base = Url::parse("https://cas.example").expect("url");
    let x = xorb(2);
    let url = signer.sign_xorb_range(&base, &x, 100, 199, NOW);
    let q = query_of(&url);

    assert_eq!(
        signer.check(&x, &q, Some("bytes=100-199"), NOW),
        Ok((100, 199))
    );

    let other_range = signer.check(&x, &q, Some("bytes=100-200"), NOW);
    let missing_range = signer.check(&x, &q, None, NOW);
    let mut tampered = q.clone();
    tampered.sig = URL_SAFE_NO_PAD.encode([0u8; 32]);
    let tampered_sig = signer.check(&x, &tampered, Some("bytes=100-199"), NOW);
    let past_exp = signer.check(&x, &q, Some("bytes=100-199"), q.exp);

    assert_eq!(
        other_range,
        Err(AuthError::Forbidden {
            reason: ForbiddenReason::RangeMismatch
        })
    );
    assert_eq!(missing_range, Err(AuthError::BadRange));
    assert_eq!(
        tampered_sig,
        Err(AuthError::Forbidden {
            reason: ForbiddenReason::BadSignature
        })
    );
    assert_eq!(
        past_exp,
        Err(AuthError::Forbidden {
            reason: ForbiddenReason::Expired
        })
    );
    let all = [other_range, missing_range, tampered_sig, past_exp];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn signed_url_bound_to_xorb_and_query_fields() {
    let signer = url_signer();
    let base = Url::parse("https://cas.example").expect("url");
    let url = signer.sign_xorb_range(&base, &xorb(3), 0, 9, NOW);
    let q = query_of(&url);
    let forbidden = Err(AuthError::Forbidden {
        reason: ForbiddenReason::BadSignature,
    });

    assert_eq!(
        signer.check(&xorb(4), &q, Some("bytes=0-9"), NOW),
        forbidden
    );

    let mut longer = q.clone();
    longer.exp += 3600;
    assert_eq!(
        signer.check(&xorb(3), &longer, Some("bytes=0-9"), NOW),
        forbidden
    );

    let mut wider = q.clone();
    wider.range = "0-10".to_owned();
    assert_eq!(
        signer.check(&xorb(3), &wider, Some("bytes=0-10"), NOW),
        forbidden
    );

    for non_canonical in ["00-9", "+0-9", "0-09", "9-0"] {
        let mut q2 = q.clone();
        q2.range = non_canonical.to_owned();
        let header = format!("bytes={non_canonical}");
        assert_eq!(
            signer.check(&xorb(3), &q2, Some(&header), NOW),
            forbidden,
            "{non_canonical}"
        );
    }

    let other_key = UrlSigner::new([8; 32], UrlSigner::DEFAULT_LIFETIME).expect("valid");
    assert_eq!(
        other_key.check(&xorb(3), &q, Some("bytes=0-9"), NOW),
        forbidden
    );
}

#[test]
fn signed_url_rejects_range_header_with_extra_spaces_or_multiple_ranges() {
    let signer = url_signer();
    let base = Url::parse("https://cas.example").expect("url");
    let x = xorb(5);
    let q = query_of(&signer.sign_xorb_range(&base, &x, 0, 1, NOW));
    for header in [
        "bytes=0-1,2-3",
        "bytes=0-1, 2-3",
        "bytes= 0-1",
        " bytes=0-1",
        "bytes=0-1 ",
        "bytes=0 - 1",
        "Bytes=0-1",
        "bytes=00-1",
        "bytes=0-",
        "bytes=-1",
    ] {
        assert_eq!(
            signer.check(&x, &q, Some(header), NOW),
            Err(AuthError::Forbidden {
                reason: ForbiddenReason::RangeMismatch
            }),
            "{header:?} must not match"
        );
    }
}

#[test]
fn url_lifetime_floor() {
    assert!(matches!(
        UrlSigner::new([1; 32], Duration::from_mins(29)),
        Err(AuthError::LifetimeTooShort { .. })
    ));
    assert!(matches!(
        UrlSigner::new([1; 32], Duration::ZERO),
        Err(AuthError::LifetimeTooShort { .. })
    ));
    let floor_error = UrlSigner::new([1; 32], Duration::from_mins(29)).unwrap_err();
    assert!(floor_error.to_string().contains("30 min"));
    assert!(UrlSigner::new([1; 32], Duration::from_mins(30)).is_ok());
    assert!(UrlSigner::new([1; 32], Duration::from_hours(1)).is_ok());
    assert_eq!(UrlSigner::DEFAULT_LIFETIME, Duration::from_hours(1));
    assert_eq!(UrlSigner::MIN_LIFETIME, Duration::from_mins(30));
}

#[test]
fn auth_errors_never_contain_the_credential() {
    let s = signer();
    let auth = Authenticator::new(
        vec![static_token(STATIC_SECRET, Mode::Read)],
        vec![s.public_key()],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    let expired = s.sign("alice", "org/model", Mode::Read, NOW, NOW);
    let foreign = signer().sign("alice", "org/model", Mode::Read, NOW + 60, NOW);
    let wrong_static = format!("{STATIC_SECRET}x");

    let us = url_signer();
    let base = Url::parse("https://cas.example").expect("url");
    let q = query_of(&us.sign_xorb_range(&base, &xorb(6), 0, 9, NOW));

    let mut cases: Vec<(String, AuthError)> = vec![
        (
            expired.clone(),
            auth.authenticate(&expired, NOW).unwrap_err(),
        ),
        (
            foreign.clone(),
            auth.authenticate(&foreign, NOW).unwrap_err(),
        ),
        (
            wrong_static.clone(),
            auth.authenticate(&wrong_static, NOW).unwrap_err(),
        ),
    ];
    for header in [None, Some("bytes=0-8")] {
        cases.push((
            q.sig.clone(),
            us.check(&xorb(6), &q, header, NOW).unwrap_err(),
        ));
    }
    cases.push((
        q.sig.clone(),
        us.check(&xorb(6), &q, Some("bytes=0-9"), q.exp)
            .unwrap_err(),
    ));
    cases.push((
        q.sig.clone(),
        us.check(&xorb(7), &q, Some("bytes=0-9"), NOW).unwrap_err(),
    ));

    for (credential, error) in cases {
        for rendered in [format!("{error:?}"), error.to_string()] {
            assert!(
                !rendered.contains(&credential),
                "{rendered} leaks credential"
            );
            assert!(!rendered.contains(STATIC_SECRET), "{rendered} leaks secret");
            assert!(!rendered.is_empty());
        }
    }
}

#[test]
fn static_token_debug_is_redacted() {
    let token = static_token(STATIC_SECRET, Mode::Read);
    let rendered = format!("{token:?}");
    assert!(!rendered.contains(STATIC_SECRET));
    assert!(!rendered.contains(&hex_of(&token.sha256)));
    assert!(rendered.contains("ci"));

    let auth = Authenticator::new(
        vec![token],
        vec![],
        Authenticator::DEFAULT_MAX_TOKEN_LIFETIME,
    )
    .expect("valid config");
    assert!(!format!("{auth:?}").contains(&hex_of(&StaticToken::digest(STATIC_SECRET))));
}

#[test]
fn secret_holders_debug_is_redacted() {
    let pair = generate_keypair().expect("generate keypair");
    let private_body = pair
        .private_pem()
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<String>();
    let s = TokenSigner::from_pem(pair.private_pem()).expect("parse");
    for rendered in [format!("{s:?}"), format!("{pair:?}")] {
        assert!(!rendered.contains(&private_body), "{rendered}");
    }

    let us = UrlSigner::new([0xab; 32], UrlSigner::DEFAULT_LIFETIME).expect("valid");
    assert!(!format!("{us:?}").contains("abab"));
    assert!(!format!("{us:?}").contains("171, 171"));

    let base = Url::parse("https://cas.example").expect("url");
    let q = query_of(&us.sign_xorb_range(&base, &xorb(8), 0, 9, NOW));
    assert!(!format!("{q:?}").contains(&q.sig));
}

fn retarget_kid(token: &str, kid: &str) -> String {
    let (header, rest) = token.split_once('.').expect("JWT has a header");
    let mut decoded: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).expect("b64")).expect("json");
    decoded["kid"] = serde_json::Value::String(kid.to_owned());
    format!("{}.{rest}", URL_SAFE_NO_PAD.encode(decoded.to_string()))
}

fn pem_body(pem: &str) -> Vec<u8> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .expect("PEM body is base64")
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    hex_of(&sha2::Sha256::digest(bytes))
}

fn hex_of(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

fn raw_keypair() -> (SigningKey, String, String) {
    let pair = generate_keypair().expect("generate keypair");
    let key = SigningKey::from_pkcs8_pem(pair.private_pem()).expect("private key");
    (key, pair.public_pem().to_owned(), pair.kid().to_owned())
}

fn valid_claims() -> serde_json::Value {
    serde_json::json!({
        "sub": "alice", "scope": "org/model", "mode": "write", "exp": NOW + 60, "iat": NOW,
    })
}

fn b64(json: &str) -> String {
    URL_SAFE_NO_PAD.encode(json)
}

/// A compact JWS over `header.payload`, signed with `key` whatever the
/// header says.
fn sign_raw(key: &SigningKey, header_b64: &str, payload_b64: &str) -> String {
    let input = format!("{header_b64}.{payload_b64}");
    let sig = key.sign(input.as_bytes());
    format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()))
}

/// Unpadded base64url (RFC 4648 §5), decoded without the `base64` crate.
fn b64url_decode_by_hand(segment: &str) -> Vec<u8> {
    let value = |c: u8| -> u32 {
        match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'-' => 62,
            b'_' => 63,
            _ => panic!("not base64url: {c}"),
        }
    };
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in segment.bytes() {
        acc = (acc << 6) | value(c);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).expect("masked to a byte"));
        }
    }
    out
}
