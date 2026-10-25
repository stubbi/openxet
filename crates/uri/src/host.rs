//! Hosts and ports (`docs/reference/xet-uri.md`, "Hosts and ports"):
//! the authority of an address, and the canonical host forms that
//! `Endpoints` compares against.

use std::net::Ipv4Addr;

use crate::{ErrorCode, UriError};

const DEFAULT_PORT: u16 = 443;

/// An authority's host and port, before the policy on single-label names
/// (which differs between the current grammar and pyxet's legacy form).
pub(crate) struct Authority {
    pub host: String,
    pub port: Option<u16>,
    pub port_written: bool,
    pub single_label: bool,
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
        let address: std::net::Ipv6Addr = inner
            .parse()
            .map_err(|_| invalid_host("the text in brackets is not an IPv6 address"))?;
        // The URL library's serialization (RFC 5952, and an IPv4-mapped
        // address in hex), so the host equals the HTTPS URL's.
        return Ok((url::Host::<String>::Ipv6(address).to_string(), false));
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
