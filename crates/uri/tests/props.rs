//! Round-trip properties of the parser and the canonical formatter
//! (`docs/reference/xet-uri.md`, "Escaping and Unicode"): every valid
//! address survives `parse(format(u)) == u`, and the canonical form of any
//! text that parses is a fixed point.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv4Addr, Ipv6Addr};
use std::str::FromStr;

use openxet_uri::{Kind, XetUri};
use proptest::prelude::*;

fn host() -> impl Strategy<Value = String> {
    prop_oneof![
        (
            prop::collection::vec("[a-z0-9_]([a-z0-9_-]{0,8}[a-z0-9_])?", 1..3),
            "[a-z]{2,6}"
        )
            .prop_map(|(labels, tld)| format!("{}.{tld}", labels.join("."))),
        Just("localhost".to_owned()),
        any::<u32>().prop_map(|ip| Ipv4Addr::from(ip).to_string()),
        any::<u128>().prop_map(|ip| format!("[{}]", Ipv6Addr::from(ip))),
    ]
}

fn kind() -> impl Strategy<Value = Kind> {
    prop_oneof![
        Just(Kind::Repository),
        Just(Kind::Dataset),
        Just(Kind::Space),
        Just(Kind::Kernel),
        Just(Kind::Bucket),
    ]
}

/// The first segments a parser reads as a kind, or refuses as one.
const KIND_WORDS: &[&str] = &[
    "models", "datasets", "spaces", "kernels", "buckets", "model", "dataset", "space", "kernel",
    "bucket",
];

fn id() -> impl Strategy<Value = String> {
    "[A-Za-z0-9][A-Za-z0-9_-]{0,12}(\\.[A-Za-z0-9_-]{1,6})?"
        .prop_filter("names don't end in .git", |id| {
            id.strip_suffix(".git").is_none()
        })
}

fn namespace() -> impl Strategy<Value = String> {
    id().prop_filter("kind words aren't namespaces", |id| {
        !KIND_WORDS.contains(&id.as_str())
    })
}

fn revision() -> impl Strategy<Value = String> {
    prop_oneof![
        "[A-Za-z0-9_-]{1,8}(/[A-Za-z0-9_-]{1,8}){0,2}",
        "[a-z0-9]{1,6}@[a-z0-9]{1,6}",
        "[a-zà-ÿ数据]{1,6}",
        "refs/pr/[0-9]{1,4}",
        "refs/convert/[a-z0-9][a-z0-9_-]{0,7}",
        "[0-9a-f]{40}",
        "[0-9a-f]{64}",
    ]
    .prop_filter("a bare `refs` before a path is ambiguous", |rev| {
        rev != "refs"
    })
}

fn path() -> impl Strategy<Value = String> {
    prop::collection::vec(
        "[^/\\x00]{1,8}".prop_filter("no dot segments", |s| s != "." && s != ".."),
        1..4,
    )
    .prop_map(|segments| segments.join("/"))
}

prop_compose! {
    fn uri()(
        host in host(),
        port in prop::option::of(1u16..).prop_map(|port| port.filter(|&p| p != 443)),
        kind in kind(),
        namespace in namespace(),
        name in id(),
        revision in prop::option::of(revision()),
        path in prop::option::of(path()),
    ) -> XetUri {
        let revision = if kind == Kind::Bucket { None } else { revision };
        XetUri { host, port, kind, namespace, name, revision, path }
    }
}

/// Renders an address the way a person might type it: scheme and host in
/// any case, and an unreserved octet of the name needlessly escaped in
/// lowercase hex.
fn spelled(uri: &XetUri, upper: bool, escape_name: bool) -> String {
    let canonical = uri.to_string();
    let authority_end = canonical["xet://".len()..].find('/').unwrap() + "xet://".len();
    let prefix = match uri.kind {
        Kind::Repository => "",
        Kind::Dataset => "datasets/",
        Kind::Space => "spaces/",
        Kind::Kernel => "kernels/",
        Kind::Bucket => "buckets/",
    };
    let name_start = authority_end + 1 + prefix.len() + uri.namespace.len() + 1;
    let mut authority = canonical[..authority_end].to_owned();
    if upper {
        authority = authority.to_uppercase();
    }
    let resource = &canonical[authority_end..name_start];
    let rest = &canonical[name_start..];
    if escape_name {
        format!(
            "{authority}{resource}%{:02x}{}",
            rest.as_bytes()[0],
            &rest[1..]
        )
    } else {
        format!("{authority}{resource}{rest}")
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    #[test]
    fn round_trip(uri in uri()) {
        let text = uri.to_string();
        prop_assert_eq!(XetUri::from_str(&text), Ok(uri), "{}", text);
    }

    #[test]
    fn canonical_is_idempotent(uri in uri(), upper: bool, escape_name: bool) {
        let text = spelled(&uri, upper, escape_name);
        let parsed = XetUri::from_str(&text).unwrap();
        let canonical = parsed.to_string();
        prop_assert_eq!(&parsed, &uri);
        prop_assert_eq!(XetUri::from_str(&canonical).unwrap().to_string(), canonical);
    }

    #[test]
    fn any_text_that_parses_has_a_fixed_point(text in "(xet|XET)://[a-zA-Z0-9.:\\[\\]@%/_~ -]{0,48}") {
        if let Ok(uri) = XetUri::from_str(&text) {
            let canonical = uri.to_string();
            let again = XetUri::from_str(&canonical).unwrap();
            prop_assert_eq!(&again, &uri);
            prop_assert_eq!(again.to_string(), canonical);
        }
    }

    #[test]
    fn https_mapping_stays_on_the_named_host(uri in uri()) {
        let url = uri.https(openxet_uri::Transport::Https);
        prop_assert_eq!(url.scheme(), "https");
        prop_assert_eq!(url.host_str().unwrap(), uri.host.as_str());
        prop_assert_eq!(url.port_or_known_default(), Some(uri.port.unwrap_or(443)));
        prop_assert!(url.username().is_empty() && url.password().is_none());
    }
}
