//! The golden vectors of `docs/reference/xet-uri.md`
//! (`testdata/vectors.json`): every valid address parses to the recorded
//! parts, formats to the recorded canonical form and maps to the recorded
//! HTTPS URL; every malformed one fails with the recorded error code.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::str::FromStr;

use openxet_uri::{Kind, Transport, XetUri};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    valid: Vec<Valid>,
    invalid: Vec<Invalid>,
}

#[derive(Deserialize)]
struct Valid {
    input: String,
    canonical: String,
    host: String,
    port: Option<u16>,
    kind: String,
    namespace: String,
    name: String,
    revision: Option<String>,
    path: Option<String>,
    immutable: bool,
    https: String,
}

#[derive(Deserialize)]
struct Invalid {
    input: String,
    code: String,
}

fn vectors() -> Vectors {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata/vectors.json");
    serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap()
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Repository => "repository",
        Kind::Dataset => "dataset",
        Kind::Space => "space",
        Kind::Kernel => "kernel",
        Kind::Bucket => "bucket",
    }
}

#[test]
fn valid_vectors_parse_format_and_map() {
    let vectors = vectors();
    for v in &vectors.valid {
        let uri = XetUri::from_str(&v.input)
            .unwrap_or_else(|err| panic!("{} should parse: {err}", v.input));
        assert_eq!(uri.host, v.host, "{}", v.input);
        assert_eq!(uri.port, v.port, "{}", v.input);
        assert_eq!(kind_name(uri.kind), v.kind, "{}", v.input);
        assert_eq!(uri.namespace, v.namespace, "{}", v.input);
        assert_eq!(uri.name, v.name, "{}", v.input);
        assert_eq!(uri.revision, v.revision, "{}", v.input);
        assert_eq!(uri.path, v.path, "{}", v.input);
        assert_eq!(uri.is_immutable(), v.immutable, "{}", v.input);
        assert_eq!(uri.to_string(), v.canonical, "{}", v.input);
        assert_eq!(
            uri.https(Transport::Https).unwrap().as_str(),
            v.https,
            "{}",
            v.input
        );
        assert_eq!(
            XetUri::from_str(&v.canonical).unwrap(),
            uri,
            "{}",
            v.canonical
        );
    }
}

#[test]
fn invalid_vectors_fail_with_their_code_and_a_hint() {
    let vectors = vectors();
    for v in &vectors.invalid {
        let err = XetUri::from_str(&v.input).expect_err(&v.input);
        assert_eq!(err.code.as_str(), v.code, "{:?}: {err}", v.input);
        assert!(!err.message.is_empty(), "{:?}", v.input);
        assert!(!err.hint.is_empty(), "{:?}", v.input);
    }
}

#[test]
fn vectors_cover_every_kind_and_every_parse_error() {
    let vectors = vectors();
    assert!(vectors.valid.len() + vectors.invalid.len() >= 40);
    let kinds: BTreeSet<&str> = vectors.valid.iter().map(|v| v.kind.as_str()).collect();
    assert_eq!(
        kinds,
        BTreeSet::from(["bucket", "dataset", "kernel", "repository", "space"])
    );
    let codes: BTreeSet<&str> = vectors.invalid.iter().map(|v| v.code.as_str()).collect();
    let parse_errors = BTreeSet::from([
        "ambiguous_revision",
        "credentials_in_uri",
        "empty_revision",
        "fragment_not_supported",
        "host_not_qualified",
        "invalid_character",
        "invalid_host",
        "invalid_name",
        "invalid_namespace",
        "invalid_path",
        "invalid_percent_encoding",
        "invalid_port",
        "invalid_revision",
        "legacy_no_host",
        "legacy_pyxet_address",
        "legacy_xethub",
        "missing_host",
        "missing_repository",
        "non_ascii_host",
        "not_xet_uri",
        "query_not_supported",
        "reserved_namespace",
        "revision_on_bucket",
        "unknown_kind",
    ]);
    assert_eq!(codes, parse_errors);
}

/// An error names what to fix, but never repeats a credential or a query
/// that may carry one.
#[test]
fn errors_never_repeat_credentials_or_queries() {
    for input in [
        "xet://user:secret-token@hub.example.org/team/corpus",
        "xet://secret-token@hub.example.org/team/corpus",
        "xet://hub.example.org/team/corpus?token=secret-token",
        "xet://hub.example.org/team/corpus#secret-token",
    ] {
        let err = XetUri::from_str(input).unwrap_err();
        let shown = format!("{err} {err:?} {}", err.hint);
        assert!(!shown.contains("secret-token"), "{input}: {shown}");
    }
}

#[test]
fn a_non_ascii_host_error_names_its_a_label() {
    let err = XetUri::from_str("xet://bücher.example/team/corpus").unwrap_err();
    assert!(
        err.hint.contains("xet://xn--bcher-kva.example/team/corpus"),
        "{}",
        err.hint
    );
}

#[test]
fn http_transport_keeps_the_named_port() {
    let uri = XetUri::from_str("xet://127.0.0.1:8080/dev/scratch/notes.txt").unwrap();
    assert_eq!(
        uri.https(Transport::Http).unwrap().as_str(),
        "http://127.0.0.1:8080/dev/scratch/resolve/main/notes.txt"
    );
    let uri = XetUri::from_str("xet://localhost/dev/scratch").unwrap();
    assert_eq!(
        uri.https(Transport::Http).unwrap().as_str(),
        "http://localhost:443/dev/scratch"
    );
}
