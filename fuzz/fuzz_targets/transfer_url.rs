//! Signed transfer URL checks on untrusted query values and `Range`
//! headers (design §5.5, ADR 0002, ruling M1-R3). Input byte 0 picks the
//! mode:
//!
//! - even: the rest is a query's `range`, `exp` and `sig` and a `Range`
//!   header. It never panics, and nothing but a valid signature passes.
//! - odd: the rest describes a range, a URL for it is signed, and the
//!   request carries some `Range` header. It passes exactly when that
//!   header is `bytes=` and the signed range, and then serves that range.

#![no_main]

use std::sync::OnceLock;
use std::time::Duration;

use libfuzzer_sys::arbitrary::{Result as ArbitraryResult, Unstructured};
use libfuzzer_sys::fuzz_target;
use openxet_auth::{TransferQuery, UrlSigner};
use openxet_protocol::{MerkleHash, chunk_hash};
use url::Url;

/// A fixed `now`, so a signed URL's validity never depends on the clock.
const NOW: u64 = 1_800_000_000;

fn signer() -> &'static UrlSigner {
    static SIGNER: OnceLock<UrlSigner> = OnceLock::new();
    SIGNER.get_or_init(|| UrlSigner::new([7; 32], Duration::from_hours(1)).expect("valid"))
}

fn xorb() -> MerkleHash {
    chunk_hash(b"fuzz")
}

fn query_of(url: &Url) -> TransferQuery {
    let value = |name: &str| {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
            .expect("a signed URL has every parameter")
    };
    TransferQuery {
        range: value("range"),
        exp: value("exp").parse().expect("a decimal exp"),
        sig: value("sig"),
    }
}

fn raw(input: &mut Unstructured<'_>) -> ArbitraryResult<()> {
    let query = TransferQuery {
        range: input.arbitrary()?,
        exp: input.arbitrary()?,
        sig: input.arbitrary()?,
    };
    let header: Option<String> = input.arbitrary()?;
    // The key is secret, so an arbitrary signature never verifies; libFuzzer
    // would have to find an HMAC-SHA256 forgery.
    assert!(
        signer()
            .check(&xorb(), &query, header.as_deref(), NOW)
            .is_err()
    );
    Ok(())
}

fn signed(input: &mut Unstructured<'_>) -> ArbitraryResult<()> {
    let start: u64 = input.arbitrary()?;
    let end = start.saturating_add(input.arbitrary()?);
    let header: Option<String> = input.arbitrary()?;
    let exact: bool = input.arbitrary()?;
    let base = Url::parse("https://cas.example").expect("a URL");
    let url = signer()
        .sign_xorb_range(&base, &xorb(), start, end, NOW)
        .expect("start <= end");
    let query = query_of(&url);
    let header = if exact {
        Some(format!("bytes={start}-{end}"))
    } else {
        header
    };
    let result = signer().check(&xorb(), &query, header.as_deref(), NOW);
    if header.as_deref() == Some(format!("bytes={start}-{end}").as_str()) {
        assert_eq!(result, Ok((start, end)));
    } else {
        assert!(result.is_err(), "{header:?} passed for {start}-{end}");
    }
    Ok(())
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    let mut input = Unstructured::new(rest);
    let _ = if mode % 2 == 0 {
        raw(&mut input)
    } else {
        signed(&mut input)
    };
});
