//! The current grammar (`docs/reference/xet-uri.md`, "Grammar"), read
//! left to right. Each refusal names its code from the specification's
//! constraint table.

use std::fmt::Write as _;
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::{ErrorCode, Kind, UriError, XetUri, legacy};

const SCHEME: &str = "xet";
const DEFAULT_PORT: u16 = 443;

/// The kind segments, and the words refused in their place: `hf://`'s
/// model view and the singular forms `hf://` refuses too. Read as a
/// namespace, they would silently name another repository.
const KINDS: &[(&str, Kind)] = &[
    ("datasets", Kind::Dataset),
    ("spaces", Kind::Space),
    ("kernels", Kind::Kernel),
    ("buckets", Kind::Bucket),
];
const NOT_KINDS: &[&str] = &["models", "model", "dataset", "space", "kernel", "bucket"];

/// The longest namespace or name, ruling M2-R2.
const MAX_ID_LEN: usize = 96;

/// An authority's host and port, before the policy on single-label names
/// (which differs between the current grammar and pyxet's legacy form).
pub(crate) struct Authority {
    pub host: String,
    pub port: Option<u16>,
    pub port_written: bool,
    pub single_label: bool,
}

pub(crate) fn parse(input: &str) -> Result<XetUri, UriError> {
    let (authority, resource) = split(input)?;
    let parsed = parse_authority(authority, resource)?;
    if is_xethub(&parsed.host) {
        return Err(legacy::xethub_error(&components(resource)));
    }
    if parsed.single_label {
        if parsed.port_written {
            return Err(host_not_qualified(&parsed.host));
        }
        let mut legacy_parts = vec![parsed.host.as_str()];
        legacy_parts.extend(components(resource));
        return Err(legacy::no_host_error(&legacy_parts));
    }
    let (kind, namespace, name, revision, path) = parse_resource(resource)?;
    Ok(XetUri {
        host: parsed.host,
        port: parsed.port,
        kind,
        namespace,
        name,
        revision,
        path,
    })
}

/// Splits `xet://{authority}{resource}`, refusing userinfo, queries and
/// fragments before anything else is read, so no error repeats them.
pub(crate) fn split(input: &str) -> Result<(&str, &str), UriError> {
    let Some((scheme, rest)) = input.split_once("://") else {
        return Err(not_xet_uri());
    };
    if !scheme.eq_ignore_ascii_case(SCHEME) {
        return Err(not_xet_uri());
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, resource) = rest.split_at(end);
    if authority.contains('@') {
        return Err(credentials_in_uri());
    }
    if let Some(at) = resource.find(['?', '#']) {
        return Err(if resource[at..].starts_with('?') {
            UriError::new(
                ErrorCode::QueryNotSupported,
                "a xet:// address has no query string (`?`)",
                "Put the revision after `@` (name@revision), never put a credential in an \
                 address, and write a literal `?` in a path as %3F.",
            )
        } else {
            UriError::new(
                ErrorCode::FragmentNotSupported,
                "a xet:// address has no fragment (`#`)",
                "Remove the `#…` part; write a literal `#` in a path as %23.",
            )
        });
    }
    Ok((authority, resource))
}

/// The path segments after the authority, for legacy hints: the empty
/// segments a trailing or leading `/` leaves are dropped.
pub(crate) fn components(resource: &str) -> Vec<&str> {
    resource.split('/').filter(|s| !s.is_empty()).collect()
}

/// Reads `host[:port]`. `resource` is only used to show a corrected
/// address in a hint.
pub(crate) fn parse_authority(authority: &str, resource: &str) -> Result<Authority, UriError> {
    let (host_text, port_text) = if let Some(inner) = authority.strip_prefix('[') {
        let Some(close) = inner.find(']') else {
            return Err(invalid_host("an IPv6 address has no closing `]`"));
        };
        let after = &inner[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(port) => Some(port),
            None if after.is_empty() => None,
            None => {
                return Err(invalid_host(
                    "only `:port` may follow an IPv6 address's `]`",
                ));
            }
        };
        (&authority[..close + 2], port)
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host_text.is_empty() {
        return Err(UriError::new(
            ErrorCode::MissingHost,
            "the address names no host",
            "Write the deployment's host after xet://, as in xet://hub.example.org/team/corpus; \
             for a server on this machine, xet://127.0.0.1:8080/….",
        ));
    }
    let port = port_text.map(parse_port).transpose()?.flatten();
    let (host, single_label) = parse_host(host_text, port, resource)?;
    Ok(Authority {
        host,
        port,
        port_written: port_text.is_some(),
        single_label,
    })
}

/// A port: decimal 1 to 65535, no leading zero. 443 is the default, so it
/// is dropped from the canonical form.
fn parse_port(text: &str) -> Result<Option<u16>, UriError> {
    let valid = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && !text.starts_with('0')
        && text.len() <= 5;
    match text.parse::<u16>() {
        Ok(DEFAULT_PORT) if valid => Ok(None),
        Ok(port) if valid => Ok(Some(port)),
        _ => Err(UriError::new(
            ErrorCode::InvalidPort,
            format!("the port {text:?} is not a port number from 1 to 65535"),
            "Write the port in decimal without leading zeros (xet://host:8443/…), or leave it \
             out for 443.",
        )),
    }
}

/// Returns the canonical host and whether it is a single-label name.
fn parse_host(text: &str, port: Option<u16>, resource: &str) -> Result<(String, bool), UriError> {
    if let Some(inner) = text.strip_prefix('[') {
        let inner = inner.strip_suffix(']').unwrap_or(inner);
        if inner.contains('%') {
            return Err(invalid_host(
                "an IPv6 zone identifier (`%25…`) names an interface of the reader's machine",
            ));
        }
        if inner.starts_with(['v', 'V']) {
            return Err(invalid_host("IPvFuture addresses are not supported"));
        }
        let address: Ipv6Addr = inner
            .parse()
            .map_err(|_| invalid_host("the text in brackets is not an IPv6 address"))?;
        return Ok((format!("[{address}]"), false));
    }
    if !text.is_ascii() {
        return Err(non_ascii_host(text, port, resource));
    }
    let host = text.to_ascii_lowercase();
    if host.contains('%') {
        return Err(invalid_host("a host can't be percent-encoded"));
    }
    if let Some(bad) = host
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
    {
        return Err(invalid_host(&format!(
            "{bad:?} can't appear in a host name"
        )));
    }
    let labels: Vec<&str> = host.split('.').collect();
    let last = labels.last().copied().unwrap_or_default();
    // WHATWG URL parsers read a name whose last label is a number as IPv4,
    // in forms (`127.1`, `0x7f.0.0.1`) other parsers don't share; only the
    // strict dotted-decimal form names one address for every reader.
    if !last.is_empty() && (last.bytes().all(|b| b.is_ascii_digit()) || last.starts_with("0x")) {
        return strict_ipv4(&labels).map(|ip| (ip.to_string(), false));
    }
    if host.len() > 253 {
        return Err(invalid_host("a host name is at most 253 characters"));
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-') {
            return Err(invalid_host(
                "each dot-separated label has 1 to 63 characters and doesn't start or end with `-` \
                 (no empty label, no trailing dot)",
            ));
        }
    }
    // The URL library's own IDNA check refuses an A-label that isn't valid
    // punycode, so every accepted host also maps to a valid HTTPS URL.
    match url::Host::parse(&host) {
        Ok(url::Host::Domain(domain)) if domain == host => {}
        _ => return Err(invalid_host("it is not a valid DNS name")),
    }
    Ok((host.clone(), labels.len() == 1 && host != "localhost"))
}

fn strict_ipv4(labels: &[&str]) -> Result<Ipv4Addr, UriError> {
    let octets: Option<Vec<u8>> = labels
        .iter()
        .map(|part| {
            let canonical = !part.is_empty()
                && part.len() <= 3
                && part.bytes().all(|b| b.is_ascii_digit())
                && (part.len() == 1 || !part.starts_with('0'));
            canonical.then(|| part.parse::<u8>().ok()).flatten()
        })
        .collect();
    match octets.as_deref() {
        Some(&[a, b, c, d]) => Ok(Ipv4Addr::new(a, b, c, d)),
        _ => Err(invalid_host(
            "an IPv4 address is four decimal numbers from 0 to 255 without leading zeros, \
             such as 127.0.0.1",
        )),
    }
}

pub(crate) fn is_xethub(host: &str) -> bool {
    host == "xethub.com" || host.ends_with(".xethub.com")
}

type Resource = (Kind, String, String, Option<String>, Option<String>);

/// Reads `/[{kind}/]{namespace}/{name}[@{revision}][/{path}]`.
fn parse_resource(resource: &str) -> Result<Resource, UriError> {
    let body = resource.strip_prefix('/').unwrap_or(resource);
    let segments: Vec<&str> = body.split('/').collect();
    let (kind, mut next) = match KINDS.iter().find(|(word, _)| *word == segments[0]) {
        Some((_, kind)) => (*kind, 1),
        None if NOT_KINDS.contains(&segments[0]) && segments.len() > 1 => {
            return Err(unknown_kind(segments[0]));
        }
        None => (Kind::Repository, 0),
    };
    let (Some(namespace_raw), Some(name_raw)) = (segments.get(next), segments.get(next + 1)) else {
        return Err(missing_repository());
    };
    if body.is_empty() || (name_raw.is_empty() && segments.len() == next + 2) {
        return Err(missing_repository());
    }
    let namespace = id(namespace_raw, ErrorCode::InvalidNamespace, "namespace")?;
    let (name_text, revision_text) = match name_raw.split_once('@') {
        Some((name, revision)) => (name, Some(revision)),
        None => (*name_raw, None),
    };
    let name = id(name_text, ErrorCode::InvalidName, "name")?;
    next += 2;

    let revision = match revision_text {
        None => None,
        Some(_) if kind == Kind::Bucket => {
            return Err(UriError::new(
                ErrorCode::RevisionOnBucket,
                "buckets have no revisions, so a bucket address has no `@`",
                "Remove `@…`: a bucket address is xet://host/buckets/{namespace}/{name}/{path}.",
            ));
        }
        Some(text) => {
            let (revision, used) = parse_revision(text, &segments[next..])?;
            next += used;
            Some(revision)
        }
    };

    let path = if next < segments.len() {
        let parts = segments[next..]
            .iter()
            .map(|segment| path_segment(segment))
            .collect::<Result<Vec<_>, _>>()?;
        Some(parts.join("/"))
    } else {
        None
    };
    Ok((kind, namespace, name, revision, path))
}

/// Reads the revision after `@`, given the segments that follow its own.
/// Returns the revision and how many of those segments it took: a
/// special ref (`refs/pr/{n}`, `refs/convert/{name}`) is written with its
/// slashes, as in `hf://` (`huggingface_hub` 2.2.0, `_hf_uris.py`
/// `_SPECIAL_REFS_REVISION_REGEX`).
fn parse_revision(text: &str, after: &[&str]) -> Result<(String, usize), UriError> {
    check_characters(text)?;
    if text.is_empty() {
        return Err(UriError::new(
            ErrorCode::EmptyRevision,
            "the address has `@` but no revision after it",
            "Write a branch, tag or commit id after `@`, or remove the `@` for the default branch.",
        ));
    }
    let revision = decode(text)?;
    if revision == "refs" {
        if text == "refs"
            && let Some(special) = special_ref(after)
        {
            validate_revision(&special)?;
            return Ok((special, 2));
        }
        if !after.is_empty() {
            return Err(UriError::new(
                ErrorCode::AmbiguousRevision,
                "a revision ends at the next `/`, so this reads as the ref `refs` followed by a \
                 path",
                "Write a ref containing `/` with %2F, as in @refs%2Fheads%2Fmain/file.txt; only \
                 refs/pr/{n} and refs/convert/{name} are written with their slashes.",
            ));
        }
    }
    validate_revision(&revision)?;
    Ok((revision, 0))
}

fn special_ref(after: &[&str]) -> Option<String> {
    match after {
        ["pr", number, ..] if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) => {
            Some(format!("refs/pr/{number}"))
        }
        ["convert", name, ..] if is_convert_name(name) => Some(format!("refs/convert/{name}")),
        _ => None,
    }
}

pub(crate) fn is_convert_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// `git check-ref-format`'s rules, one-level names allowed
/// (<https://git-scm.com/docs/git-check-ref-format>). A commit id passes
/// them too.
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Git compares ref names byte for byte: `.LOCK` is a valid ending"
)]
fn validate_revision(revision: &str) -> Result<(), UriError> {
    let reason = if let Some(bad) = revision
        .chars()
        .find(|c| c.is_ascii_control() || " ~^:?*[\\".contains(*c))
    {
        Some(format!("it contains {bad:?}"))
    } else if revision.contains("..") {
        Some("it contains `..`".to_owned())
    } else if revision.contains("@{") {
        Some("it contains `@{`".to_owned())
    } else if revision == "@" {
        Some("`@` alone is not a ref".to_owned())
    } else if revision.starts_with('/') || revision.ends_with('/') || revision.contains("//") {
        Some("it has a leading, trailing or doubled `/`".to_owned())
    } else if revision.ends_with('.') {
        Some("it ends with `.`".to_owned())
    } else if revision
        .split('/')
        .any(|part| part.starts_with('.') || part.ends_with(".lock"))
    {
        Some("a part starts with `.` or ends with `.lock`".to_owned())
    } else {
        None
    };
    match reason {
        None => Ok(()),
        Some(reason) => Err(UriError::new(
            ErrorCode::InvalidRevision,
            format!("{revision:?} is not a Git ref name or commit id: {reason}"),
            "Check the branch, tag or commit id (`git check-ref-format` has the rules), and \
             write a `/` inside it as %2F.",
        )),
    }
}

/// A namespace or name: `[A-Za-z0-9][A-Za-z0-9._-]{0,95}`, no `..`, and a
/// name doesn't end in `.git` (ruling M2-R2).
#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "ruling M2-R2 refuses the suffix `.git` as written"
)]
fn id(raw: &str, code: ErrorCode, what: &str) -> Result<String, UriError> {
    check_characters(raw)?;
    let id = decode(raw)?;
    let mut bytes = id.bytes();
    let valid = bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && id.len() <= MAX_ID_LEN
        && !id.contains("..")
        && !(code == ErrorCode::InvalidName && id.ends_with(".git"));
    if valid {
        return Ok(id);
    }
    let shown = if id.is_empty() {
        "an empty segment".to_owned()
    } else {
        format!("{id:?}")
    };
    Err(UriError::new(
        code,
        format!("{shown} is not a valid repository {what}"),
        format!(
            "A {what} starts with a letter or digit and has up to 96 letters, digits, `.`, `_` \
             and `-`, with no `..`{}; `@` can't be part of one, since it starts a revision.",
            if code == ErrorCode::InvalidName {
                ", and doesn't end in .git"
            } else {
                ""
            },
        ),
    ))
}

fn path_segment(raw: &str) -> Result<String, UriError> {
    let invalid = |why: &str| {
        UriError::new(
            ErrorCode::InvalidPath,
            format!("the path is not valid: {why}"),
            "Write the path as segments separated by single `/`, with no `.` or `..` segment \
             and no trailing `/`; a path names a file or a folder either way.",
        )
    };
    if raw.is_empty() {
        return Err(invalid("it has an empty segment (a trailing `/` or `//`)"));
    }
    check_characters(raw)?;
    let segment = decode(raw)?;
    if segment == "." || segment == ".." {
        return Err(invalid("`.` and `..` segments are not resolved"));
    }
    if segment.contains('/') {
        return Err(invalid(
            "an encoded `/` (%2F) inside a segment; write `/` between segments",
        ));
    }
    if segment.contains('\0') {
        return Err(invalid("it contains NUL (%00)"));
    }
    Ok(segment)
}

/// Refuses raw characters that RFC 3986 doesn't allow in a path segment;
/// non-ASCII characters are accepted as in an IRI (RFC 3987 §3.1).
fn check_characters(raw: &str) -> Result<(), UriError> {
    let allowed =
        |c: char| !c.is_ascii() || c.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:@%".contains(c);
    match raw.chars().find(|c| !allowed(*c)) {
        None => Ok(()),
        Some(bad) => Err(UriError::new(
            ErrorCode::InvalidCharacter,
            format!("{bad:?} can't appear unescaped in a xet:// address"),
            format!("Percent-encode it as {}.", percent_encoded(bad)),
        )),
    }
}

fn percent_encoded(c: char) -> String {
    let mut buf = [0; 4];
    c.encode_utf8(&mut buf)
        .bytes()
        .fold(String::new(), |mut out, b| {
            let _ = write!(out, "%{b:02X}");
            out
        })
}

/// Percent-decodes `raw`; the decoded octets must be UTF-8.
fn decode(raw: &str) -> Result<String, UriError> {
    let invalid = |why: &str| {
        UriError::new(
            ErrorCode::InvalidPercentEncoding,
            format!("the percent-encoding is not valid: {why}"),
            "Write each escaped octet as `%` and two hex digits, and escape whole UTF-8 \
             characters (é is %C3%A9).",
        )
    };
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .filter(|_| bytes[i + 1].is_ascii_hexdigit() && bytes[i + 2].is_ascii_hexdigit());
            let Some(octet) = hex else {
                return Err(invalid("`%` is not followed by two hex digits"));
            };
            out.push(octet);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| invalid("the decoded octets are not UTF-8"))
}

fn not_xet_uri() -> UriError {
    UriError::new(
        ErrorCode::NotXetUri,
        "this is not a xet:// address",
        "Write it as xet://{host}/{namespace}/{name}[@{revision}][/{path}]; an HTTPS link is \
         used as it is, not as a xet:// address.",
    )
}

fn credentials_in_uri() -> UriError {
    UriError::new(
        ErrorCode::CredentialsInUri,
        "a xet:// address never carries a user name, password or token (`…@host`)",
        "Remove everything between xet:// and `@`, and configure the credential for the \
         endpoint instead (hf auth login, or the client's token setting).",
    )
}

pub(crate) fn invalid_host(why: &str) -> UriError {
    UriError::new(
        ErrorCode::InvalidHost,
        format!("the host is not valid: {why}"),
        "Write the deployment's DNS name (hub.example.org), an IPv4 address (127.0.0.1) or an \
         IPv6 address in brackets ([::1]).",
    )
}

pub(crate) fn host_not_qualified(host: &str) -> UriError {
    UriError::new(
        ErrorCode::HostNotQualified,
        format!(
            "{host:?} is a single-label host name, which each reader's network resolves \
             differently"
        ),
        format!(
            "Write the fully qualified name ({host}.example.org, say) or an IP address; only \
             localhost may be a single label."
        ),
    )
}

fn non_ascii_host(text: &str, port: Option<u16>, resource: &str) -> UriError {
    let port = port.map(|p| format!(":{p}")).unwrap_or_default();
    match url::Host::parse(text) {
        Ok(url::Host::Domain(a_label)) => UriError::new(
            ErrorCode::NonAsciiHost,
            format!("the host {text:?} is not ASCII"),
            format!("Write the host as its A-label: xet://{a_label}{port}{resource}"),
        ),
        _ => invalid_host("it is not a valid internationalised domain name"),
    }
}

fn missing_repository() -> UriError {
    UriError::new(
        ErrorCode::MissingRepository,
        "the address names no repository or bucket",
        "Write {namespace}/{name} after the host (and after the kind, if any), as in \
         xet://hub.example.org/team/corpus.",
    )
}

fn unknown_kind(word: &str) -> UriError {
    UriError::new(
        ErrorCode::UnknownKind,
        format!("{word:?} is not a kind in a xet:// address"),
        "Address a repository without a kind (xet://host/{namespace}/{name}), or write one of \
         datasets/, spaces/, kernels/ or buckets/ (plural).",
    )
}
