//! Credential selection for `xet://` addresses (ADR 0009 item 4,
//! `docs/reference/xet-uri.md` "Credentials and endpoints"), at the
//! library level: two endpoint fixtures hold the same names with different
//! content and different credentials, and each address reaches only its
//! own host, with only its own credential. No network is used.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use openxet_uri::{Credential, Endpoints, Request, XetUri};

const TOKEN_A: &str = "token-for-a-only";
const TOKEN_B: &str = "token-for-b-only";

/// A deployment: its origin, the one credential it accepts, and its files
/// by HTTPS path.
struct Fixture {
    origin: &'static str,
    token: &'static str,
    files: &'static [(&'static str, &'static str)],
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        origin: "https://a.example.org",
        token: TOKEN_A,
        files: &[("/team/corpus/resolve/main/data.txt", "content held by a")],
    },
    Fixture {
        origin: "https://b.example.org:8443",
        token: TOKEN_B,
        files: &[("/team/corpus/resolve/main/data.txt", "content held by b")],
    },
];

/// What a fixture answers: its file when the request carries its own
/// credential. Every credential a request carries is also recorded with
/// the origin it went to, so a test can check none crossed.
fn serve(request: &Request, seen: &mut Vec<(String, String)>) -> Result<&'static str, u16> {
    let origin = request.url().origin().ascii_serialization();
    if let Some(credential) = request.credential() {
        seen.push((origin.clone(), credential.expose().to_owned()));
    }
    let fixture = FIXTURES
        .iter()
        .find(|f| f.origin == origin)
        .ok_or(404_u16)?;
    if request.credential().map(Credential::expose) != Some(fixture.token) {
        return Err(401);
    }
    fixture
        .files
        .iter()
        .find(|(path, _)| *path == request.url().path())
        .map(|(_, content)| *content)
        .ok_or(404)
}

fn endpoints() -> Endpoints {
    let mut endpoints = Endpoints::new();
    endpoints
        .add_default("https://a.example.org", Some(Credential::new(TOKEN_A)))
        .unwrap();
    endpoints
        .add("https://b.example.org:8443", Some(Credential::new(TOKEN_B)))
        .unwrap();
    endpoints
}

#[test]
fn endpoints_isolate_credentials() {
    let endpoints = endpoints();
    let mut seen = Vec::new();

    // Resolution picks the address's host, and the same names hold each
    // host's own content.
    let a = XetUri::from_str("xet://a.example.org/team/corpus/data.txt").unwrap();
    let b = XetUri::from_str("xet://b.example.org:8443/team/corpus/data.txt").unwrap();
    let to_a = endpoints.resolve(&a).unwrap();
    let to_b = endpoints.resolve(&b).unwrap();
    assert_eq!(
        to_a.url().as_str(),
        "https://a.example.org/team/corpus/resolve/main/data.txt"
    );
    assert_eq!(
        to_b.url().as_str(),
        "https://b.example.org:8443/team/corpus/resolve/main/data.txt"
    );
    assert_eq!(serve(&to_a, &mut seen), Ok("content held by a"));
    assert_eq!(serve(&to_b, &mut seen), Ok("content held by b"));

    // An embedded credential is refused, and the refusal doesn't repeat it.
    let err =
        XetUri::from_str("xet://user:token-for-a-only@b.example.org:8443/team/corpus").unwrap_err();
    assert_eq!(err.code.as_str(), "credentials_in_uri");
    assert!(!format!("{err} {} {err:?}", err.hint).contains(TOKEN_A));

    // The default endpoint's credential is never sent to another host: an
    // address on a host with no endpoint goes out without a credential, and
    // so does one on a configured host but another port.
    for other in [
        "xet://c.example.org/team/corpus/data.txt",
        "xet://a.example.org:8443/team/corpus/data.txt",
        "xet://b.example.org/team/corpus/data.txt",
    ] {
        let request = endpoints
            .resolve(&XetUri::from_str(other).unwrap())
            .unwrap();
        assert!(request.credential().is_none(), "{other}");
        assert_eq!(request.url().scheme(), "https", "{other}");
    }

    // A redirect to another host doesn't carry the credential, even to a
    // host the client holds a credential for.
    let moved = to_a
        .redirect("https://b.example.org:8443/team/corpus/resolve/main/data.txt")
        .unwrap();
    assert!(moved.credential().is_none());
    assert_eq!(serve(&moved, &mut seen), Err(401));

    for (origin, token) in &seen {
        let fixture = FIXTURES.iter().find(|f| f.origin == *origin).unwrap();
        assert_eq!(
            token, fixture.token,
            "{origin} received another endpoint's credential"
        );
    }
}

#[test]
fn redirects_never_broaden_authority() {
    let endpoints = endpoints();
    let request = endpoints
        .resolve(&XetUri::from_str("xet://a.example.org/team/corpus/x.bin").unwrap())
        .unwrap();

    // Same origin, relative or absolute: the credential stays, as for the
    // file gateway's redirect (ruling M2-R9).
    let gateway = request
        .redirect("/v1/transfer/files/abc/def?name=x.bin")
        .unwrap();
    assert_eq!(gateway.credential().map(Credential::expose), Some(TOKEN_A));
    assert_eq!(
        gateway.url().as_str(),
        "https://a.example.org/v1/transfer/files/abc/def?name=x.bin"
    );
    let absolute = request.redirect("https://a.example.org:443/other").unwrap();
    assert_eq!(absolute.credential().map(Credential::expose), Some(TOKEN_A));

    // Another host, another port, or a downgrade to HTTP: dropped.
    for location in [
        "https://storage.example.net/bucket/object?X-Amz-Signature=abc",
        "https://a.example.org:8443/other",
        "http://a.example.org/other",
        "https://evil.example/a.example.org",
    ] {
        let next = request.redirect(location).unwrap();
        assert!(next.credential().is_none(), "{location}");
        // Once dropped, it doesn't come back on a redirect home.
        let home = next
            .redirect("https://a.example.org/team/corpus/x.bin")
            .unwrap();
        assert!(home.credential().is_none(), "{location}");
    }

    // A Location with userinfo, or one that isn't HTTP(S), is refused.
    let err = request
        .redirect("https://user:sneaky@a.example.org/x")
        .unwrap_err();
    assert_eq!(err.code.as_str(), "credentials_in_uri");
    assert!(!format!("{err} {}", err.hint).contains("sneaky"));
    assert_eq!(
        request
            .redirect("file:///etc/passwd")
            .unwrap_err()
            .code
            .as_str(),
        "invalid_redirect"
    );
}

#[test]
fn unconfigured_loopback_is_plain_http_and_anonymous() {
    let endpoints = endpoints();
    for (address, url) in [
        (
            "xet://127.0.0.1:8080/dev/scratch/a.txt",
            "http://127.0.0.1:8080/dev/scratch/resolve/main/a.txt",
        ),
        (
            "xet://localhost:8080/dev/scratch",
            "http://localhost:8080/dev/scratch",
        ),
        (
            "xet://[::1]:8080/dev/scratch",
            "http://[::1]:8080/dev/scratch",
        ),
    ] {
        let request = endpoints
            .resolve(&XetUri::from_str(address).unwrap())
            .unwrap();
        assert_eq!(request.url().as_str(), url);
        assert!(request.credential().is_none());
    }
}

#[test]
fn a_configured_endpoint_selects_transport_and_credential_by_exact_host_and_port() {
    let mut endpoints = Endpoints::new();
    endpoints
        .add(
            "http://localhost:8080",
            Some(Credential::new("local-dev-token")),
        )
        .unwrap();
    endpoints.add("https://[::1]:9443", None).unwrap();

    let local = endpoints
        .resolve(&XetUri::from_str("xet://localhost:8080/dev/scratch").unwrap())
        .unwrap();
    assert_eq!(local.url().as_str(), "http://localhost:8080/dev/scratch");
    assert_eq!(
        local.credential().map(Credential::expose),
        Some("local-dev-token")
    );

    // No alias: 127.0.0.1 is not localhost.
    let numeric = endpoints
        .resolve(&XetUri::from_str("xet://127.0.0.1:8080/dev/scratch").unwrap())
        .unwrap();
    assert!(numeric.credential().is_none());

    let v6 = endpoints
        .resolve(&XetUri::from_str("xet://[0:0:0:0:0:0:0:1]:9443/dev/scratch").unwrap())
        .unwrap();
    assert_eq!(v6.url().as_str(), "https://[::1]:9443/dev/scratch");
}

#[test]
fn endpoint_configuration_refuses_base_paths_userinfo_and_duplicates() {
    let mut endpoints = Endpoints::new();
    let err = endpoints.add("https://example.org/hub/", None).unwrap_err();
    assert_eq!(err.code.as_str(), "base_path_unsupported");
    assert!(!err.hint.is_empty());

    let err = endpoints
        .add("https://user:very-secret@example.org", None)
        .unwrap_err();
    assert_eq!(err.code.as_str(), "credentials_in_uri");
    assert!(!format!("{err} {} {err:?}", err.hint).contains("very-secret"));

    for bad in [
        "ftp://example.org",
        "not a url",
        "https://example.org/?x=1",
        "https://example.org/#x",
    ] {
        assert_eq!(
            endpoints.add(bad, None).unwrap_err().code.as_str(),
            "invalid_endpoint",
            "{bad}"
        );
    }

    endpoints.add("https://example.org", None).unwrap();
    let err = endpoints.add("https://EXAMPLE.org:443/", None).unwrap_err();
    assert_eq!(err.code.as_str(), "duplicate_endpoint");
    endpoints
        .add_default("https://other.example.org", None)
        .unwrap();
    let err = endpoints
        .add_default("https://third.example.org", None)
        .unwrap_err();
    assert_eq!(err.code.as_str(), "duplicate_endpoint");
    assert_eq!(
        endpoints.default_endpoint().unwrap().url().as_str(),
        "https://other.example.org/"
    );
}

#[test]
fn credentials_never_show_in_debug_output() {
    let endpoints = endpoints();
    let request = endpoints
        .resolve(&XetUri::from_str("xet://a.example.org/team/corpus").unwrap())
        .unwrap();
    let shown = format!("{endpoints:?} {request:?}");
    assert!(
        !shown.contains(TOKEN_A) && !shown.contains(TOKEN_B),
        "{shown}"
    );
}

#[test]
fn a_credential_goes_over_plain_http_only_to_loopback() {
    let mut endpoints = Endpoints::new();
    let err = endpoints
        .add(
            "http://hub.example.org:8080",
            Some(Credential::new("plain-http-token")),
        )
        .unwrap_err();
    assert_eq!(err.code.as_str(), "credential_over_http");
    assert!(!format!("{err} {} {err:?}", err.hint).contains("plain-http-token"));
    endpoints.add("http://hub.example.org:8080", None).unwrap();
    for local in [
        "http://localhost:8080",
        "http://127.0.0.1:8080",
        "http://[::1]:8080",
    ] {
        endpoints
            .add(local, Some(Credential::new("local-dev-token")))
            .unwrap();
    }
}

#[test]
fn an_ipv4_mapped_address_matches_its_endpoint() {
    let mut endpoints = Endpoints::new();
    endpoints
        .add(
            "https://[::ffff:1.2.3.4]",
            Some(Credential::new("mapped-token")),
        )
        .unwrap();
    let uri = XetUri::from_str("xet://[::ffff:1.2.3.4]/team/corpus").unwrap();
    let request = endpoints.resolve(&uri).unwrap();
    assert_eq!(request.url().host_str(), Some(uri.host.as_str()));
    assert_eq!(
        request.credential().map(Credential::expose),
        Some("mapped-token")
    );
}
