//! The connection check before a transfer, and the proxy in the way.

use std::io;
use std::time::Duration;

use hyper_util::client::proxy::matcher::Matcher;

use crate::{ClientError, Endpoint};

/// How long the connection check waits for the endpoint (or the proxy) to
/// accept.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The proxy the client core sends requests to `ep` through, without
/// credentials, as reqwest picks it: `HTTP_PROXY`, `HTTPS_PROXY`,
/// `ALL_PROXY` and `NO_PROXY`, else the system settings (reqwest's
/// `system-proxy` feature, which xet-client builds with).
pub(crate) fn proxy_for(ep: &Endpoint) -> Option<http::Uri> {
    let uri: http::Uri = ep.url.parse().ok()?;
    Matcher::from_system()
        .intercept(&uri)
        .map(|intercept| intercept.uri().clone())
}

/// `scheme://host:port` of a proxy, never its credentials.
pub(crate) fn shown(proxy: &http::Uri) -> String {
    match authority(proxy) {
        Some((host, port)) => format!("{}://{host}:{port}", proxy.scheme_str().unwrap_or("http")),
        None => "the configured proxy".to_owned(),
    }
}

/// Fails fast, before the client core's retries (about 90 s for a refused
/// connection), when the token has expired or nothing listens where
/// requests go: the endpoint, or the proxy in front of it.
pub(crate) async fn preflight(ep: &Endpoint, proxy: Option<&http::Uri>) -> Result<(), ClientError> {
    ep.check_expiry()?;
    let target: http::Uri = match proxy {
        Some(proxy) => proxy.clone(),
        None => ep.url.parse().map_err(|_| ClientError::InvalidEndpoint {
            reason: "it is not a URL",
        })?,
    };
    let Some((host, port)) = authority(&target) else {
        return Ok(());
    };
    let unreachable = |reason: String| ClientError::Unreachable {
        endpoint: ep.url.clone(),
        proxy: proxy.map(shown),
        reason,
    };
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) => Err(unreachable(connect_error(&err))),
        Err(_) => Err(unreachable(format!(
            "no answer within {}s",
            CONNECT_TIMEOUT.as_secs()
        ))),
    }
}

/// The host and port a URI dials, with the scheme's default port.
fn authority(uri: &http::Uri) -> Option<(String, u16)> {
    let default_port = match uri.scheme_str() {
        Some("https") => 443,
        Some(s) if s.starts_with("socks") => 1080,
        _ => 80,
    };
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']');
    Some((host.to_owned(), uri.port_u16().unwrap_or(default_port)))
}

fn connect_error(err: &io::Error) -> String {
    match err.kind() {
        io::ErrorKind::ConnectionRefused => "the connection was refused".to_owned(),
        io::ErrorKind::TimedOut => "the connection timed out".to_owned(),
        _ => err.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn hosts_and_ports_have_scheme_defaults() {
        let at = |s: &str| authority(&s.parse().unwrap());
        assert_eq!(
            at("https://cas.example.com"),
            Some(("cas.example.com".into(), 443))
        );
        assert_eq!(at("http://[::1]:9000"), Some(("::1".into(), 9000)));
        assert_eq!(at("socks5://p"), Some(("p".into(), 1080)));
    }

    #[test]
    fn a_shown_proxy_has_no_credentials() {
        let proxy: http::Uri = "http://user:oxt_secret-marker@proxy.example:3128"
            .parse()
            .unwrap();
        assert_eq!(shown(&proxy), "http://proxy.example:3128");
    }
}
