//! The current grammar (`docs/reference/xet-uri.md`, "Grammar"), read
//! left to right. Each refusal names its code from the specification's
//! constraint table.

use std::fmt::Write as _;

use crate::host::{host_not_qualified, is_xethub, parse_authority};
use crate::{ErrorCode, Kind, UriError, XetUri, legacy};

const SCHEME: &str = "xet";

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

/// Namespaces no repository can take (ruling M2-R2), compared ignoring
/// ASCII case: the kind words, and the first segments of openxet's own
/// routes.
const RESERVED_NAMESPACES: &[&str] = &[
    "models",
    "datasets",
    "spaces",
    "kernels",
    "buckets",
    "api",
    "v1",
    "v2",
    "openapi.json",
    "healthz",
    "readyz",
];

/// The longest namespace or name, ruling M2-R2.
const MAX_ID_LEN: usize = 96;

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

type Resource = (Kind, String, String, Option<String>, Option<String>);

/// Reads `/[{kind}/]{namespace}/{name}[@{revision}][/{path}]`.
fn parse_resource(resource: &str) -> Result<Resource, UriError> {
    let body = resource.strip_prefix('/').unwrap_or(resource);
    let segments: Vec<&str> = body.split('/').collect();
    // The first segment is matched decoded, so that no spelling of a kind
    // word is ever read as a namespace; only the literal lowercase word is
    // a kind, so the canonical form reads back the same.
    check_characters(segments[0])?;
    let first = decode(segments[0])?;
    let kind_word = KINDS.iter().find(|(word, _)| *word == first);
    if (kind_word.is_some() || NOT_KINDS.contains(&first.as_str()))
        && first != segments[0]
        && segments.len() > 1
    {
        return Err(UriError::new(
            ErrorCode::UnknownKind,
            format!("{first:?} is written with percent-encoding, which a kind never is"),
            format!(
                "Write it plainly: {first}/ for a kind, or address the repository without one."
            ),
        ));
    }
    let (kind, mut next) = match kind_word {
        Some((_, kind)) => (*kind, 1),
        None if NOT_KINDS.contains(&first.as_str()) && segments.len() > 1 => {
            return Err(unknown_kind(&first));
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
    if RESERVED_NAMESPACES
        .iter()
        .any(|word| word.eq_ignore_ascii_case(&namespace))
    {
        return Err(UriError::new(
            ErrorCode::ReservedNamespace,
            format!("{namespace:?} is a reserved namespace, which no repository or bucket has"),
            "Check the address: a kind is written in lowercase (datasets/, spaces/, kernels/, \
             buckets/), and a repository needs a namespace of its own.",
        ));
    }
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

/// Whether `text` is a namespace or name by its characters and length:
/// `[A-Za-z0-9][A-Za-z0-9._-]{0,95}` without `..` (ruling M2-R2).
pub(crate) fn is_id(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && text.len() <= MAX_ID_LEN
        && !text.contains("..")
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
    if is_id(&id) && !(code == ErrorCode::InvalidName && id.ends_with(".git")) {
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
