//! The legacy policy of `docs/reference/xet-uri.md` ("Legacy forms"), row
//! by row: XetHub-era Git-Xet and pyxet addresses are accepted, translated
//! on their own host, or refused, and never sent to a default server.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use openxet_uri::{Legacy, XetUri, classify_legacy};

enum Want {
    Accepted(&'static str),
    Translated(&'static str),
    Rejected(&'static str),
}

/// The matrix rows L1 to L20, in the specification's order.
const MATRIX: &[(&str, &str, Want)] = &[
    (
        "L1",
        "xet://hub.example.org/team/corpus@main/a.txt",
        Want::Accepted("xet://hub.example.org/team/corpus@main/a.txt"),
    ),
    (
        "L2",
        "xet://xethub.com/user/repo/branch/hello/world",
        Want::Rejected("legacy_xethub"),
    ),
    (
        "L3",
        "xet://xethub.com:user/repo/main/data.csv",
        Want::Rejected("legacy_xethub"),
    ),
    (
        "L4",
        "xet://www.xethub.com/user/repo",
        Want::Rejected("legacy_xethub"),
    ),
    (
        "L5",
        "xet://user/repo/branch/hello/world",
        Want::Rejected("legacy_no_host"),
    ),
    (
        "L6",
        "xet://user/repo/branch/",
        Want::Rejected("legacy_no_host"),
    ),
    ("L7", "xet://user/repo", Want::Rejected("legacy_no_host")),
    ("L8", "xet://user", Want::Rejected("legacy_no_host")),
    (
        "L9",
        "xet://:user/repo/main",
        Want::Rejected("legacy_no_host"),
    ),
    (
        "L10",
        "xet://hub.example.org:user/repo/main/a/b.csv",
        Want::Translated("xet://hub.example.org/user/repo@main/a/b.csv"),
    ),
    (
        "L11",
        "xet://localhost:8080:user/repo/main/x",
        Want::Translated("xet://localhost:8080/user/repo@main/x"),
    ),
    (
        "L12",
        "xet://hub.example.org:user/repo/main/dir/",
        Want::Translated("xet://hub.example.org/user/repo@main/dir"),
    ),
    (
        "L13",
        "xet://hub.example.org:user/repo",
        Want::Translated("xet://hub.example.org/user/repo"),
    ),
    (
        "L14",
        "xet://hub.example.org:user",
        Want::Rejected("invalid_port"),
    ),
    (
        "L15",
        "xet://storage:user/repo/main",
        Want::Rejected("host_not_qualified"),
    ),
    (
        "L16",
        "/user/repo/branch/path",
        Want::Rejected("not_xet_uri"),
    ),
    (
        "L17",
        "https://xethub.com/user/repo/branch/path",
        Want::Rejected("not_xet_uri"),
    ),
    (
        "L18",
        "xet://example.com/user/repo/branch/path",
        Want::Accepted("xet://example.com/user/repo/branch/path"),
    ),
    (
        "L19",
        "xet://hub.example.org:8080/user/repo/main",
        Want::Accepted("xet://hub.example.org:8080/user/repo/main"),
    ),
    (
        "L20",
        "xet://localhost/user/repo",
        Want::Accepted("xet://localhost/user/repo"),
    ),
];

#[test]
fn legacy_matrix() {
    for (row, input, want) in MATRIX {
        let got = classify_legacy(input);
        let parsed = XetUri::from_str(input);
        match (want, got) {
            (Want::Accepted(canonical), Legacy::Accepted(uri)) => {
                assert_eq!(uri.to_string(), *canonical, "{row}");
                assert_eq!(parsed.unwrap(), uri, "{row}");
            }
            (Want::Translated(canonical), Legacy::Translated { from, to, note }) => {
                assert_eq!(from, *input, "{row}");
                assert_eq!(to.to_string(), *canonical, "{row}");
                assert!(!note.is_empty(), "{row}");
                // The parser never applies a translation silently: it
                // refuses, and its hint carries the translated address.
                let err = parsed.unwrap_err();
                assert_eq!(err.code.as_str(), "legacy_pyxet_address", "{row}");
                assert!(err.hint.contains(canonical), "{row}: {}", err.hint);
            }
            (Want::Rejected(code), Legacy::Rejected(err)) => {
                assert_eq!(err.code.as_str(), *code, "{row}: {err}");
                assert!(!err.hint.is_empty(), "{row}");
                assert_eq!(parsed.unwrap_err(), err, "{row}");
            }
            (_, got) => panic!("{row} {input}: unexpected {got:?}"),
        }
    }
}

#[test]
fn a_hostless_address_is_rewritten_around_a_host_the_reader_supplies() {
    let Legacy::Rejected(err) = classify_legacy("xet://user/repo/branch/hello/world") else {
        panic!("a host-less address is refused");
    };
    assert!(
        err.hint
            .contains("xet://<host>/user/repo@branch/hello/world"),
        "{}",
        err.hint
    );
    let Legacy::Rejected(err) = classify_legacy("xet://user/repo/branch/") else {
        panic!("a host-less address is refused");
    };
    assert!(
        err.hint.contains("xet://<host>/user/repo@branch"),
        "{}",
        err.hint
    );
    assert!(!err.hint.contains("@branch/"), "{}", err.hint);
}

#[test]
fn a_dropped_trailing_slash_is_reported() {
    let Legacy::Translated { note, .. } =
        classify_legacy("xet://hub.example.org:user/repo/main/dir/")
    else {
        panic!("pyxet's explicit-endpoint form is translated");
    };
    assert!(note.contains("trailing"), "{note}");
}

#[test]
fn xethub_is_refused_in_every_form() {
    for input in [
        "xet://xethub.com/team/corpus@main/a.txt",
        "xet://XetHub.com/user/repo",
        "xet://xethub.com:8443/user/repo",
        "xet://hub.xethub.com/user/repo",
    ] {
        let Legacy::Rejected(err) = classify_legacy(input) else {
            panic!("{input} is refused");
        };
        assert_eq!(err.code.as_str(), "legacy_xethub", "{input}");
    }
}
