//! Capability `href` signatures: one scope, one operation, one expiry,
//! signed with the transfer key under their own domain string (ADR 0003
//! decision 2, ADR 0005 item 10, ruling M2-R3).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openxet_auth::{
    AuthError, CapabilityQuery, CapabilityRefusal, CapabilitySigner, Mode, TransferQuery, UrlSigner,
};
use url::Url;

const NOW: u64 = 1_800_000_000;
const EXP: u64 = NOW + 3600;
const KEY: [u8; 32] = [9; 32];
const SCOPE: &str = "repo:0192f0c4a1b27c3d8e9f00112233aabb";

fn signer() -> CapabilitySigner {
    CapabilitySigner::new(KEY).unwrap()
}

fn refused(reason: CapabilityRefusal) -> Result<(), AuthError> {
    Err(AuthError::CapabilityRefused { reason })
}

#[test]
fn sign_then_verify() {
    let signer = signer();
    for op in [Mode::Read, Mode::Write] {
        let query = signer.sign(SCOPE, op, EXP);
        assert_eq!(query.exp, EXP);
        assert_eq!(signer.verify(SCOPE, op, &query, NOW), Ok(()));
        assert_eq!(signer.verify(SCOPE, op, &query, EXP - 1), Ok(()));
    }
    // Deterministic: every node holding the key signs the same `sig`.
    assert_eq!(
        CapabilitySigner::new(KEY)
            .unwrap()
            .sign(SCOPE, Mode::Write, EXP)
            .sig,
        signer.sign(SCOPE, Mode::Write, EXP).sig
    );
    let sig = URL_SAFE_NO_PAD
        .decode(signer.sign(SCOPE, Mode::Write, EXP).sig)
        .unwrap();
    assert_eq!(sig.len(), 32, "an unpadded base64url HMAC-SHA256");
}

#[test]
fn wrong_scope_or_op_or_exp_fails() {
    let signer = signer();
    let query = signer.sign(SCOPE, Mode::Read, EXP);
    let bad = refused(CapabilityRefusal::BadSignature);
    assert_eq!(signer.verify("repo:other", Mode::Read, &query, NOW), bad);
    assert_eq!(signer.verify(SCOPE, Mode::Write, &query, NOW), bad);
    let later = CapabilityQuery {
        exp: EXP + 1,
        sig: query.sig.clone(),
    };
    assert_eq!(signer.verify(SCOPE, Mode::Read, &later, NOW), bad);
    assert_eq!(
        CapabilitySigner::new([8; 32])
            .unwrap()
            .verify(SCOPE, Mode::Read, &query, NOW),
        bad,
        "another key"
    );
    for sig in ["", "!!!", "AAAA", &format!("{}=", query.sig)] {
        let garbled = CapabilityQuery {
            exp: EXP,
            sig: sig.to_owned(),
        };
        assert_eq!(
            signer.verify(SCOPE, Mode::Read, &garbled, NOW),
            bad,
            "{sig:?}"
        );
    }
}

#[test]
fn expired_fails() {
    let signer = signer();
    let query = signer.sign(SCOPE, Mode::Write, EXP);
    let expired = refused(CapabilityRefusal::Expired);
    assert_eq!(signer.verify(SCOPE, Mode::Write, &query, EXP), expired);
    assert_eq!(signer.verify(SCOPE, Mode::Write, &query, EXP + 1), expired);
    // A forged signature is reported as forged, not as expired, so the
    // answer never tells a guesser that only the time was wrong.
    let forged = CapabilityQuery {
        exp: NOW - 1,
        sig: signer.sign(SCOPE, Mode::Write, EXP).sig,
    };
    assert_eq!(
        signer.verify(SCOPE, Mode::Write, &forged, NOW),
        refused(CapabilityRefusal::BadSignature)
    );
}

/// The comparison is `subtle`'s `ConstantTimeEq` over the full 32-byte
/// tags (see `CapabilitySigner::verify`), so a wrong signature differing in
/// its first byte and one differing in its last take the same path and
/// give the same answer.
#[test]
fn sig_is_constant_time_compared() {
    let signer = signer();
    let good = URL_SAFE_NO_PAD
        .decode(signer.sign(SCOPE, Mode::Write, EXP).sig)
        .unwrap();
    let flip = |at: usize| {
        let mut sig = good.clone();
        sig[at] ^= 1;
        CapabilityQuery {
            exp: EXP,
            sig: URL_SAFE_NO_PAD.encode(sig),
        }
    };
    let (first, last) = (flip(0), flip(good.len() - 1));
    assert_eq!(first.sig.len(), last.sig.len());
    let bad = refused(CapabilityRefusal::BadSignature);
    assert_eq!(signer.verify(SCOPE, Mode::Write, &first, NOW), bad);
    assert_eq!(signer.verify(SCOPE, Mode::Write, &last, NOW), bad);
}

#[test]
fn query_round_trips_through_url() {
    let signer = signer();
    let query = signer.sign(SCOPE, Mode::Write, EXP);
    let mut url = Url::parse("https://openxet.example/v1/capabilities/0192f0c4/write").unwrap();
    url.set_query(Some(&serde_urlencoded::to_string(&query).unwrap()));
    let parsed: CapabilityQuery = serde_urlencoded::from_str(url.query().unwrap()).unwrap();
    assert_eq!(parsed, query);
    assert_eq!(signer.verify(SCOPE, Mode::Write, &parsed, NOW), Ok(()));
    assert!(
        !format!("{query:?}").contains(&query.sig),
        "Debug never prints the signature"
    );
}

/// The transfer key signs both transfer URLs and capabilities; their
/// domain strings differ, so neither signature verifies as the other.
#[test]
fn capability_and_transfer_signatures_never_verify_as_each_other() {
    let urls = UrlSigner::new(KEY, UrlSigner::DEFAULT_LIFETIME).unwrap();
    let xorb = openxet_protocol::chunk_hash(b"x");
    let base = Url::parse("https://openxet.example").unwrap();
    let url = urls.sign_xorb_range(&base, &xorb, 0, 9, NOW).unwrap();
    let transfer: TransferQuery = serde_urlencoded::from_str(url.query().unwrap()).unwrap();
    let as_capability = CapabilityQuery {
        exp: transfer.exp,
        sig: transfer.sig.clone(),
    };
    let hex = openxet_protocol::hash_to_hex(&xorb);
    for scope in [hex.as_str(), "xorb", ""] {
        for op in [Mode::Read, Mode::Write] {
            assert!(signer().verify(scope, op, &as_capability, NOW).is_err());
        }
    }

    let capability = signer().sign(&hex, Mode::Read, NOW + 60);
    let as_transfer = TransferQuery {
        range: "0-9".to_owned(),
        exp: capability.exp,
        sig: capability.sig,
    };
    assert!(
        urls.check(&xorb, &as_transfer, Some("bytes=0-9"), NOW)
            .is_err()
    );
}
