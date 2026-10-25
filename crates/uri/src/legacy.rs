//! The legacy policy (`docs/reference/xet-uri.md`, "Legacy forms";
//! evidence in `docs/research/xet-uri-legacy-audit-2026-10.md`).
//!
//! XetHub's Git-Xet 0.14.5 read every host but `xethub.com` as a user on
//! the server `XET_ENDPOINT` named; pyxet 0.2.0-rc1 wrote the endpoint
//! before a colon, or defaulted to `xethub.com` without one. An address
//! that names no server is never sent to a default one (ADR 0009 item 2).

use crate::parse::{self, Authority};
use crate::{ErrorCode, UriError, XetUri};

/// How an input relates to the current grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Legacy {
    /// Valid under the current grammar, read with the current meaning.
    Accepted(XetUri),
    /// A legacy form that names its host, rewritten on that same host.
    /// Parsing refuses it; a caller may offer the translation.
    Translated {
        /// The input, as given.
        from: String,
        /// The same resource in the current grammar.
        to: XetUri,
        /// What changed, for a person to check.
        note: String,
    },
    /// Refused: malformed, or a legacy form that names no usable server.
    Rejected(UriError),
}

/// Classifies `input`: an address under the current grammar, pyxet's
/// explicit-endpoint form (translated on its own host), or a refusal that
/// says how to write the address.
#[must_use]
pub fn classify_legacy(input: &str) -> Legacy {
    if let Some((endpoint, user, after)) = pyxet_endpoint_form(input) {
        return match pyxet_translation(endpoint, user, after) {
            Ok((to, trailing_slash)) => {
                let mut note = format!(
                    "pyxet wrote the endpoint before a colon (xet://{{endpoint}}:{{user}}/…); \
                     {to} names the same repository, branch and path on the same host."
                );
                if trailing_slash {
                    note.push_str(
                        " The trailing `/` pyxet kept on folder paths is dropped: a path names \
                         a file or a folder either way.",
                    );
                }
                Legacy::Translated {
                    from: input.to_owned(),
                    to,
                    note,
                }
            }
            Err(err) => Legacy::Rejected(err),
        };
    }
    match parse::parse(input) {
        Ok(uri) => Legacy::Accepted(uri),
        Err(err) => Legacy::Rejected(err),
    }
}

/// Splits pyxet's `xet://{endpoint}:{user}{after}`: the authority's last
/// `:` is followed by something that isn't a port number. pyxet split at
/// the last colon too (`url_parsing.py`, `netloc.rsplit(":", maxsplit=1)`),
/// so `localhost:8080:user` is endpoint `localhost:8080`.
fn pyxet_endpoint_form(input: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = input.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("xet") {
        return None;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, after) = rest.split_at(end);
    if authority.contains('@') {
        return None;
    }
    let (endpoint, user) = if authority.starts_with('[') {
        let close = authority.find(']')?;
        let user = authority[close + 1..].strip_prefix(':')?;
        (&authority[..=close], user)
    } else {
        authority.rsplit_once(':')?
    };
    if user.is_empty() || user.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((endpoint, user, after))
}

/// Builds the translation and parses it back, so it is valid under the
/// current grammar. Returns whether a trailing `/` was dropped.
fn pyxet_translation(endpoint: &str, user: &str, after: &str) -> Result<(XetUri, bool), UriError> {
    // A query or fragment is refused with the grammar's own errors.
    parse::split(&format!("xet://h{after}"))?;
    let trailing_slash = after.len() > 1 && after.ends_with('/');
    let rest = if trailing_slash {
        &after[..after.len() - 1]
    } else {
        after
    };
    let rest: Vec<&str> = rest
        .strip_prefix('/')
        .map_or_else(Vec::new, |r| r.split('/').collect());
    let mut parts = vec![user];
    parts.extend(rest.iter().copied());
    if endpoint.is_empty() {
        return Err(no_host_error(&parts));
    }
    let Authority {
        host,
        port,
        single_label,
        ..
    } = parse::parse_authority(endpoint, after)?;
    if parse::is_xethub(&host) {
        return Err(xethub_error(&parts));
    }
    if single_label {
        return Err(parse::host_not_qualified(&host));
    }
    let authority = match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };
    let Some((repo, after_repo)) = rest.split_first() else {
        return Err(UriError::new(
            ErrorCode::MissingRepository,
            "this pyxet address names a user but no repository",
            format!("Write the repository after the user: xet://{authority}/{user}/<name>."),
        ));
    };
    // pyxet's names never contained `@`, which starts a revision here.
    if repo.contains('@') {
        return Err(UriError::new(
            ErrorCode::InvalidName,
            "a pyxet repository name contains `@`",
            "Repository names can't contain `@`; check the address.",
        ));
    }
    // pyxet's branch is one segment ("branches with '/' are not
    // supported"). Only a branch named `refs` followed by a path doesn't
    // translate faithfully: it would read as a special ref or be refused
    // as ambiguous.
    if after_repo.first() == Some(&"refs") && after_repo.len() > 1 {
        return Err(UriError::new(
            ErrorCode::AmbiguousRevision,
            "this pyxet address has a branch named `refs` followed by a path, which has no \
             unambiguous translation",
            "Address the files through another branch, tag or commit id.",
        ));
    }
    let mut text = format!("xet://{authority}/{user}/{repo}");
    if let Some(branch) = after_repo.first() {
        text.push('@');
        text.push_str(branch);
    }
    if after_repo.len() > 1 {
        text.push('/');
        text.push_str(&after_repo[1..].join("/"));
    }
    Ok((parse::parse(&text)?, trailing_slash))
}

/// The refusal of an address that names no server: Git-Xet's and
/// pyxet's colon-less forms, read as `{user}/{repo}/{branch}/{path}`.
pub(crate) fn no_host_error(parts: &[&str]) -> UriError {
    let user = parts.first().copied().unwrap_or_default();
    UriError::new(
        ErrorCode::LegacyNoHost,
        format!(
            "{user:?} is not a host name, so this address names no server; XetHub's Git-Xet and \
             pyxet read addresses like it against a default server (XET_ENDPOINT, or \
             xethub.com), and openxet never sends an address to a default server"
        ),
        format!(
            "Write the server's host explicitly: {}, with <host> the openxet deployment that \
             holds the data (for a server on this machine, 127.0.0.1 and its port).",
            rewrite(parts)
        ),
    )
}

/// The refusal of an address naming XetHub's former service.
pub(crate) fn xethub_error(parts: &[&str]) -> UriError {
    UriError::new(
        ErrorCode::LegacyXethub,
        "xethub.com was XetHub's service; no openxet deployment answers there, and where the \
         data lives now can't be inferred from the address",
        format!(
            "Write the address with the host of the deployment that holds the data now: {}.",
            rewrite(parts)
        ),
    )
}

/// The current-grammar form of legacy components `{user}/{repo}/{branch}/
/// {path}`, around a `<host>` the reader supplies. Components that already
/// use `@` are kept as written.
fn rewrite(parts: &[&str]) -> String {
    if parts.iter().any(|p| p.contains('@')) {
        return format!("xet://<host>/{}", parts.join("/"));
    }
    match parts {
        [] => "xet://<host>/<namespace>/<name>".to_owned(),
        [user] => format!("xet://<host>/{user}/<name>"),
        [user, repo] => format!("xet://<host>/{user}/{repo}"),
        [user, repo, branch] => format!("xet://<host>/{user}/{repo}@{branch}"),
        [user, repo, branch, path @ ..] => {
            format!("xet://<host>/{user}/{repo}@{branch}/{}", path.join("/"))
        }
    }
}
