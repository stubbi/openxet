//! Repository names (ruling M2-R2, ADR 0012 item 4).

use openxet_index::Repo;

use crate::HubError;

/// Namespaces that are route prefixes, where no repository may be created
/// (ADR 0012 item 4).
pub const RESERVED_NAMESPACES: [&str; 11] = [
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

/// The longest namespace or name (ruling M2-R2).
const MAX_SEGMENT: usize = 96;

const SEGMENT_HINT: &str = "A namespace and a name each match ^[A-Za-z0-9][A-Za-z0-9._-]{0,95}$: \
     1 to 96 letters, digits, `.`, `_` and `-`, starting with a letter or digit (ruling M2-R2).";

const DOTS_HINT: &str = "A namespace or name may not contain `..` (ruling M2-R2); choose one \
     without it.";

const GIT_SUFFIX_HINT: &str = "A name may not end in `.git`, which Git URLs append (ruling \
     M2-R2); drop the suffix.";

const RESERVED_HINT: &str = "The namespaces models, datasets, spaces, kernels, buckets, api, v1, \
     v2, openapi.json, healthz and readyz are route prefixes (ADR 0012); choose another \
     namespace.";

/// Checks `{namespace}/{name}` against ruling M2-R2.
///
/// # Errors
///
/// [`HubError::InvalidName`] naming the part and the rule it breaks.
pub fn check_name(namespace: &str, name: &str) -> Result<(), HubError> {
    check_segment("namespace", namespace)?;
    check_segment("name", name)?;
    if name.len() >= 4 && name[name.len() - 4..].eq_ignore_ascii_case(".git") {
        return Err(invalid(
            format!("the name {name:?} ends in `.git`"),
            GIT_SUFFIX_HINT,
        ));
    }
    Ok(())
}

fn check_segment(what: &str, segment: &str) -> Result<(), HubError> {
    let wrong = if segment.is_empty() {
        "is empty"
    } else if segment.len() > MAX_SEGMENT {
        "is longer than 96 characters"
    } else if !segment.as_bytes()[0].is_ascii_alphanumeric() {
        "does not start with a letter or digit"
    } else if !segment
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        "holds a character other than letters, digits, `.`, `_` and `-`"
    } else if segment.contains("..") {
        return Err(invalid(
            format!("the {what} {segment:?} contains `..`"),
            DOTS_HINT,
        ));
    } else {
        return Ok(());
    };
    Err(invalid(
        format!("the {what} {segment:?} {wrong}"),
        SEGMENT_HINT,
    ))
}

fn invalid(message: String, hint: &'static str) -> HubError {
    HubError::InvalidName { message, hint }
}

/// `{namespace}/{name}` as stored.
pub(crate) fn full_name(repo: &Repo) -> String {
    format!("{}/{}", repo.namespace, repo.name)
}

/// Checks that `namespace` isn't a route prefix (ADR 0012 item 4).
///
/// # Errors
///
/// [`HubError::InvalidName`] naming the reserved namespace.
pub(crate) fn check_not_reserved(namespace: &str) -> Result<(), HubError> {
    if RESERVED_NAMESPACES
        .iter()
        .any(|r| r.eq_ignore_ascii_case(namespace))
    {
        return Err(invalid(
            format!("the namespace {namespace:?} is reserved"),
            RESERVED_HINT,
        ));
    }
    Ok(())
}
