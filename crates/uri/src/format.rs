//! The canonical form and the HTTPS mapping (`docs/reference/xet-uri.md`,
//! "Escaping and Unicode" and "The HTTPS mapping").

use std::fmt;
use std::fmt::Write as _;

use url::Url;

use crate::parse::is_convert_name;
use crate::{Kind, Transport, XetUri};

/// The revision an address without one names (ruling M2-R5), as
/// `huggingface_hub`'s `to_url` defaults to it.
const DEFAULT_BRANCH: &str = "main";

impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Repository => "",
            Self::Dataset => "datasets/",
            Self::Space => "spaces/",
            Self::Kernel => "kernels/",
            Self::Bucket => "buckets/",
        }
    }
}

impl fmt::Display for XetUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "xet://{}", self.host)?;
        if let Some(port) = self.port.filter(|&port| port != 443) {
            write!(f, ":{port}")?;
        }
        write!(
            f,
            "/{}{}/{}",
            self.kind.prefix(),
            encode(&self.namespace, false),
            encode(&self.name, false)
        )?;
        if let Some(revision) = &self.revision {
            write!(f, "@{}", encode_revision(revision))?;
        }
        if let Some(path) = &self.path {
            write!(f, "/{}", encode(path, true))?;
        }
        Ok(())
    }
}

impl XetUri {
    /// The HTTPS URL of the resource on the same host and port, through
    /// the routes `huggingface_hub` uses: the repository or bucket page, a
    /// revision's `tree`, or a file's `resolve`.
    ///
    /// # Panics
    ///
    /// Only for a value built by hand whose host is not a valid host;
    /// every parsed value maps.
    #[must_use]
    pub fn https(&self, transport: Transport) -> Url {
        let scheme = match transport {
            Transport::Https => "https",
            Transport::Http => "http",
        };
        let mut url = format!(
            "{scheme}://{}:{}/{}{}/{}",
            self.host,
            self.port.unwrap_or(443),
            self.kind.prefix(),
            encode(&self.namespace, false),
            encode(&self.name, false)
        );
        let revision = self.revision.as_deref();
        // Writing to a String can't fail.
        let _ = match (self.kind, revision, &self.path) {
            (Kind::Bucket, _, Some(path)) => write!(url, "/resolve/{}", encode(path, true)),
            (_, Some(revision), None) => write!(url, "/tree/{}", encode_revision(revision)),
            (_, revision, Some(path)) => write!(
                url,
                "/resolve/{}/{}",
                encode_revision(revision.unwrap_or(DEFAULT_BRANCH)),
                encode(path, true)
            ),
            (_, None, None) => Ok(()),
        };
        Url::parse(&url).unwrap_or_else(|err| {
            panic!(
                "the address's host {:?} is not a valid URL host: {err}",
                self.host
            )
        })
    }
}

/// A special ref is written with its slashes, as `hf://` and
/// `huggingface_hub`'s `to_url` write it; any other `/` is `%2F`.
fn encode_revision(revision: &str) -> String {
    let special = revision
        .strip_prefix("refs/pr/")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        || revision
            .strip_prefix("refs/convert/")
            .is_some_and(is_convert_name);
    if special {
        revision.to_owned()
    } else {
        encode(revision, false)
    }
}

/// Writes every octet outside RFC 3986's `unreserved` as `%XX`, uppercase;
/// `/` stays when `keep_slash`.
fn encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'.' | b'_' | b'~')
            || (keep_slash && byte == b'/')
        {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}
