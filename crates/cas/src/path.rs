//! Path segments shared by the routes that name a hash under a prefix
//! (S10 "Conventions for every route").

use axum::extract::{FromRequestParts as _, RawPathParams};
use axum::http::request::Parts;
use openxet_protocol::{MerkleHash, hash_from_hex};

use crate::{AppState, CasError};

/// The hash of a `{prefix}/{hash}` route whose prefix is one of
/// `prefixes`, so that every object has exactly one URL:
/// - a path with any percent-encoding is `400 invalid_path`. Clients send
///   prefixes and hex hashes as they are, and axum would decode
///   `%64efault` to `default`, giving one object a second URL.
/// - any other prefix is `404 not_found`, as for any route not served
/// - a hash not in the spec's form is `400 invalid_hash`
///
/// None depends on the caller, so all are answered before authentication.
pub(crate) async fn prefixed_hash(
    parts: &mut Parts,
    state: &AppState,
    prefixes: &[&str],
) -> Result<MerkleHash, CasError> {
    // The raw request path: `RawPathParams` holds segments already
    // percent-decoded (axum 0.8.9, `extract/path/mod.rs`).
    if parts.uri.path().contains('%') {
        return Err(CasError::InvalidPath);
    }
    let params = RawPathParams::from_request_parts(parts, state)
        .await
        .map_err(|_| CasError::NoRoute)?;
    let (mut prefix, mut hash) = (None, None);
    for (name, value) in &params {
        match name {
            "prefix" => prefix = Some(value),
            "hash" => hash = Some(value),
            _ => {}
        }
    }
    if !prefix.is_some_and(|p| prefixes.contains(&p)) {
        return Err(CasError::NoRoute);
    }
    parse_hash(hash.unwrap_or_default())
}

/// A path hash: exactly 64 lowercase hex characters (S10 "Conventions for
/// every route"; <https://huggingface.co/docs/xet/api#converting-hashes-to-strings>).
/// `hash_from_hex` alone also takes uppercase, which no client sends and
/// which would give one object two URLs.
fn parse_hash(s: &str) -> Result<MerkleHash, CasError> {
    if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(CasError::InvalidHash);
    }
    hash_from_hex(s).map_err(|_| CasError::InvalidHash)
}
