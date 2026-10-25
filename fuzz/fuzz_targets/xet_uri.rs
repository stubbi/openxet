//! The `xet://` parser and the legacy classifier on untrusted text
//! (`docs/reference/xet-uri.md`). Neither panics; every address either
//! accepts formats to a canonical form that parses back to the same
//! address and is a fixed point, and maps to an HTTP(S) URL on its own
//! host; a refusal carries a hint and never repeats the userinfo it
//! refused.

#![no_main]

use std::str::FromStr;

use libfuzzer_sys::fuzz_target;
use openxet_uri::{ErrorCode, Legacy, Transport, XetUri, classify_legacy};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    match XetUri::from_str(text) {
        Ok(uri) => check(&uri),
        Err(err) => {
            assert!(!err.hint.is_empty());
            if err.code == ErrorCode::CredentialsInUri {
                let userinfo = userinfo(text);
                // Short userinfo occurs in the fixed text by chance.
                if userinfo.len() >= 8 {
                    assert!(!err.message.contains(userinfo) && !err.hint.contains(userinfo));
                }
            }
        }
    }
    match classify_legacy(text) {
        Legacy::Accepted(uri) | Legacy::Translated { to: uri, .. } => check(&uri),
        Legacy::Rejected(err) => assert!(!err.hint.is_empty()),
    }
});

fn check(uri: &XetUri) {
    let canonical = uri.to_string();
    let again = XetUri::from_str(&canonical).expect("a canonical address parses");
    assert_eq!(&again, uri);
    assert_eq!(again.to_string(), canonical);
    for transport in [Transport::Https, Transport::Http] {
        let url = uri.https(transport);
        assert_eq!(url.host_str(), Some(uri.host.as_str()));
        assert!(url.username().is_empty() && url.password().is_none());
    }
}

fn userinfo(text: &str) -> &str {
    let rest = text.split_once("://").map_or(text, |(_, rest)| rest);
    let authority = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    authority.rsplit_once('@').map_or("", |(userinfo, _)| userinfo)
}
