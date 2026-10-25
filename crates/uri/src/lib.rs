//! The `xet://` resource address (ADR 0009, M2-URI): one parser, one
//! canonical formatter, the HTTPS mapping, the legacy XetHub policy and
//! credential selection by exact endpoint.
//!
//! The grammar, its errors and its golden vectors are specified in
//! `docs/reference/xet-uri.md`; this crate is that page's implementation,
//! and it does no I/O.
//!
//! ```
//! use openxet_uri::{Kind, Transport, XetUri};
//!
//! let uri: XetUri = "xet://hub.example.org/datasets/team/corpus@v2/train.csv".parse()?;
//! assert_eq!(uri.kind, Kind::Dataset);
//! assert_eq!(uri.revision.as_deref(), Some("v2"));
//! assert_eq!(
//!     uri.https(Transport::Https).as_str(),
//!     "https://hub.example.org/datasets/team/corpus/resolve/v2/train.csv",
//! );
//! # Ok::<(), openxet_uri::UriError>(())
//! ```

mod format;
mod legacy;
mod parse;
mod resolve;

use std::str::FromStr;

pub use legacy::{Legacy, classify_legacy};
pub use resolve::{Credential, Endpoint, Endpoints, Request};

/// A parsed `xet://` address. Every value the parser returns is valid, and
/// formats (`Display`) to its canonical form, which parses back to the
/// same value.
///
/// The fields are decoded: `revision` and `path` hold the text a ref or a
/// file is named by, not its percent-encoding.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct XetUri {
    /// The deployment's host, lowercase; an IPv6 address in brackets, in
    /// its RFC 5952 form.
    pub host: String,
    /// The port, when it isn't 443, the default.
    pub port: Option<u16>,
    /// The view the address goes through.
    pub kind: Kind,
    /// The repository's or bucket's namespace (ruling M2-R2).
    pub namespace: String,
    /// The repository's or bucket's name (ruling M2-R2).
    pub name: String,
    /// A branch, tag or commit id; `None` is the default branch. Buckets
    /// have none.
    pub revision: Option<String>,
    /// A file or folder inside the resource, segments joined by `/`.
    pub path: Option<String>,
}

/// The view an address goes through (ADR 0010, design §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    /// The repository itself, whatever its labels (no kind segment).
    Repository,
    /// The repository's `dataset` view (`datasets/`).
    Dataset,
    /// The repository's `space` view (`spaces/`).
    Space,
    /// The repository's `kernel` view (`kernels/`).
    Kernel,
    /// The bucket primitive (`buckets/`), resolved from M5 on.
    Bucket,
}

/// How a deployment is reached: HTTPS, or plain HTTP for a local server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Transport {
    Https,
    Http,
}

/// Why an address, an endpoint or a redirect was refused. It never
/// carries a credential or a query string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct UriError {
    pub code: ErrorCode,
    /// What is wrong.
    pub message: String,
    /// How to write it instead.
    pub hint: String,
}

/// The stable error codes of `docs/reference/xet-uri.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    NotXetUri,
    CredentialsInUri,
    MissingHost,
    InvalidHost,
    HostNotQualified,
    NonAsciiHost,
    InvalidPort,
    MissingRepository,
    UnknownKind,
    InvalidNamespace,
    InvalidName,
    EmptyRevision,
    InvalidRevision,
    AmbiguousRevision,
    RevisionOnBucket,
    InvalidPath,
    InvalidPercentEncoding,
    InvalidCharacter,
    QueryNotSupported,
    FragmentNotSupported,
    LegacyXethub,
    LegacyNoHost,
    LegacyPyxetAddress,
    InvalidEndpoint,
    BasePathUnsupported,
    DuplicateEndpoint,
    InvalidRedirect,
}

impl ErrorCode {
    /// The code as written in errors, JSON and the specification.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotXetUri => "not_xet_uri",
            Self::CredentialsInUri => "credentials_in_uri",
            Self::MissingHost => "missing_host",
            Self::InvalidHost => "invalid_host",
            Self::HostNotQualified => "host_not_qualified",
            Self::NonAsciiHost => "non_ascii_host",
            Self::InvalidPort => "invalid_port",
            Self::MissingRepository => "missing_repository",
            Self::UnknownKind => "unknown_kind",
            Self::InvalidNamespace => "invalid_namespace",
            Self::InvalidName => "invalid_name",
            Self::EmptyRevision => "empty_revision",
            Self::InvalidRevision => "invalid_revision",
            Self::AmbiguousRevision => "ambiguous_revision",
            Self::RevisionOnBucket => "revision_on_bucket",
            Self::InvalidPath => "invalid_path",
            Self::InvalidPercentEncoding => "invalid_percent_encoding",
            Self::InvalidCharacter => "invalid_character",
            Self::QueryNotSupported => "query_not_supported",
            Self::FragmentNotSupported => "fragment_not_supported",
            Self::LegacyXethub => "legacy_xethub",
            Self::LegacyNoHost => "legacy_no_host",
            Self::LegacyPyxetAddress => "legacy_pyxet_address",
            Self::InvalidEndpoint => "invalid_endpoint",
            Self::BasePathUnsupported => "base_path_unsupported",
            Self::DuplicateEndpoint => "duplicate_endpoint",
            Self::InvalidRedirect => "invalid_redirect",
        }
    }
}

impl UriError {
    pub(crate) fn new(
        code: ErrorCode,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
        }
    }
}

impl XetUri {
    /// Whether the address names content that can't change: a repository
    /// at a full commit id (40 or 64 lowercase hex digits). An abbreviated
    /// id is a ref name, never expanded.
    #[must_use]
    pub fn is_immutable(&self) -> bool {
        self.kind != Kind::Bucket
            && self.revision.as_deref().is_some_and(|rev| {
                matches!(rev.len(), 40 | 64)
                    && rev
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
    }
}

/// Parses an address under the current grammar. A legacy XetHub form is
/// never rewritten silently: it is refused, and a translatable one carries
/// its translation in the hint (see [`classify_legacy`]).
impl FromStr for XetUri {
    type Err = UriError;

    fn from_str(input: &str) -> Result<Self, UriError> {
        match classify_legacy(input) {
            Legacy::Accepted(uri) => Ok(uri),
            Legacy::Translated { to, note, .. } => Err(UriError::new(
                ErrorCode::LegacyPyxetAddress,
                "this is a pyxet address (xet://{endpoint}:{user}/{repo}/{branch}/{path}), \
                 which openxet doesn't read as written",
                format!("Write {to} instead. {note}"),
            )),
            Legacy::Rejected(err) => Err(err),
        }
    }
}
