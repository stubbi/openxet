//! Credential selection (ADR 0009 item 4, `docs/reference/xet-uri.md`
//! "Credentials and endpoints"): a credential belongs to one endpoint,
//! chosen by the address's exact host and port, never by default, and a
//! redirect can only keep or drop it.

use std::fmt;
use std::net::Ipv4Addr;

use url::Url;

use crate::{ErrorCode, Transport, UriError, XetUri};

/// A secret for one endpoint. It never appears in `Debug` output.
#[derive(Clone, PartialEq, Eq)]
pub struct Credential(String);

impl Credential {
    #[must_use]
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    /// The secret, for the `Authorization` header and nothing else.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credential(<redacted>)")
    }
}

/// A deployment the client is configured for: its base URL, at the root of
/// its host, and the credential it accepts.
#[derive(Debug, Clone)]
pub struct Endpoint {
    url: Url,
    host: String,
    port: u16,
    transport: Transport,
    credential: Option<Credential>,
}

impl Endpoint {
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub fn credential(&self) -> Option<&Credential> {
        self.credential.as_ref()
    }
}

/// The endpoints a client is configured for. At most one is the default,
/// for commands that take no address; an address always selects its own.
#[derive(Debug, Clone, Default)]
pub struct Endpoints {
    endpoints: Vec<Endpoint>,
    default: Option<usize>,
}

/// One HTTP request to make: where, and with which credential, if any.
#[derive(Debug, Clone)]
pub struct Request {
    url: Url,
    credential: Option<Credential>,
}

impl Endpoints {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds the endpoint at `base_url` (`https://host[:port]`, or `http://`
    /// for a local server) with its credential.
    ///
    /// # Errors
    ///
    /// `invalid_endpoint` unless it is an `http` or `https` URL without a
    /// query or fragment; `credentials_in_uri` when it carries userinfo;
    /// `base_path_unsupported` when it has a path; `credential_over_http` for
    /// a credential on plain HTTP to a host that isn't loopback;
    /// `duplicate_endpoint` when its host and port are already configured.
    pub fn add(&mut self, base_url: &str, credential: Option<Credential>) -> Result<(), UriError> {
        let endpoint = endpoint(base_url, credential)?;
        if self
            .endpoints
            .iter()
            .any(|e| e.host == endpoint.host && e.port == endpoint.port)
        {
            return Err(UriError::new(
                ErrorCode::DuplicateEndpoint,
                format!("an endpoint for {} is already configured", endpoint.url),
                "Configure each host and port once, with one credential.",
            ));
        }
        self.endpoints.push(endpoint);
        Ok(())
    }

    /// Adds the endpoint as [`add`](Self::add) does and makes it the
    /// default: the endpoint of commands that take no address. Its
    /// credential still goes only to its own host.
    ///
    /// # Errors
    ///
    /// As [`add`](Self::add), and `duplicate_endpoint` when a default is
    /// already set.
    pub fn add_default(
        &mut self,
        base_url: &str,
        credential: Option<Credential>,
    ) -> Result<(), UriError> {
        if self.default.is_some() {
            return Err(UriError::new(
                ErrorCode::DuplicateEndpoint,
                "a default endpoint is already configured",
                "Configure one default endpoint.",
            ));
        }
        self.add(base_url, credential)?;
        self.default = Some(self.endpoints.len() - 1);
        Ok(())
    }

    #[must_use]
    pub fn default_endpoint(&self) -> Option<&Endpoint> {
        self.default.map(|i| &self.endpoints[i])
    }

    /// The request for `uri`: on the endpoint configured for its exact host
    /// and port, with that endpoint's transport and credential. Without one,
    /// the request carries no credential, and goes over HTTPS, or plain HTTP
    /// to a loopback host (local development).
    ///
    /// # Errors
    ///
    /// As [`XetUri::https`]: only for an address built by hand with an
    /// invalid host.
    pub fn resolve(&self, uri: &XetUri) -> Result<Request, UriError> {
        let port = uri.port.unwrap_or(443);
        Ok(
            match self
                .endpoints
                .iter()
                .find(|e| e.host == uri.host && e.port == port)
            {
                Some(endpoint) => Request {
                    url: uri.https(endpoint.transport)?,
                    credential: endpoint.credential.clone(),
                },
                None => Request {
                    url: uri.https(if is_loopback(&uri.host) {
                        Transport::Http
                    } else {
                        Transport::Https
                    })?,
                    credential: None,
                },
            },
        )
    }
}

impl Request {
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    #[must_use]
    pub fn credential(&self) -> Option<&Credential> {
        self.credential.as_ref()
    }

    /// The request a redirect to `location` (absolute, or relative to this
    /// request's URL) leads to. It keeps the credential only on the same
    /// origin (scheme, host and port): the server can't choose which of the
    /// client's credentials it receives, so no other is ever attached.
    ///
    /// # Errors
    ///
    /// `invalid_redirect` when `location` is not an `http` or `https` URL;
    /// `credentials_in_uri` when it carries userinfo. Neither repeats it.
    pub fn redirect(&self, location: &str) -> Result<Request, UriError> {
        let invalid = || {
            UriError::new(
                ErrorCode::InvalidRedirect,
                "the server redirected to a location that is not an http or https URL",
                "Report it to the deployment's operator; the request was not followed.",
            )
        };
        let next = self.url.join(location).map_err(|_| invalid())?;
        if !matches!(next.scheme(), "http" | "https") {
            return Err(invalid());
        }
        if !next.username().is_empty() || next.password().is_some() {
            return Err(UriError::new(
                ErrorCode::CredentialsInUri,
                "the server redirected to a location with a user name or password in it",
                "Report it to the deployment's operator; the request was not followed.",
            ));
        }
        let credential = if next.origin() == self.url.origin() {
            self.credential.clone()
        } else {
            None
        };
        Ok(Request {
            url: next,
            credential,
        })
    }
}

fn endpoint(base_url: &str, credential: Option<Credential>) -> Result<Endpoint, UriError> {
    // No error repeats `base_url`: it may hold userinfo.
    let invalid = |why: &str| {
        UriError::new(
            ErrorCode::InvalidEndpoint,
            format!("an endpoint is not valid: {why}"),
            "Configure the endpoint as https://host[:port], or http://127.0.0.1:port for a \
             local server.",
        )
    };
    let url = Url::parse(base_url).map_err(|_| invalid("it is not an absolute URL"))?;
    let transport = match url.scheme() {
        "https" => Transport::Https,
        "http" => Transport::Http,
        _ => return Err(invalid("its scheme is not http or https")),
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UriError::new(
            ErrorCode::CredentialsInUri,
            "an endpoint URL carries a user name or password",
            "Remove it from the URL and configure the credential for the endpoint separately.",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid("it has a query or fragment"));
    }
    if url.path() != "/" {
        return Err(UriError::new(
            ErrorCode::BasePathUnsupported,
            format!(
                "the endpoint {} has a base path, {:?}",
                origin(&url),
                url.path()
            ),
            "Serve openxet at the root of its own host name (hub.example.org rather than \
             example.org/hub/): a xet:// address has no room for a base path. Base paths are \
             revisited with endpoint discovery in M6.",
        ));
    }
    let (Some(host), Some(port)) = (url.host(), url.port_or_known_default()) else {
        return Err(invalid("it has no host"));
    };
    // The URL library's serialization, which the parser gives an
    // address's host too.
    let host = host.to_string();
    // A credential over plain HTTP crosses the network readable, except to
    // this machine (local development).
    if transport == Transport::Http && credential.is_some() && !is_loopback(&host) {
        return Err(UriError::new(
            ErrorCode::CredentialOverHttp,
            format!(
                "the endpoint {} is plain HTTP, which would send its credential unencrypted",
                origin(&url)
            ),
            "Serve the endpoint over HTTPS, or configure it without a credential; plain HTTP \
             carries a credential only to a loopback host (localhost, 127.0.0.1, [::1]).",
        ));
    }
    Ok(Endpoint {
        url,
        host,
        port,
        transport,
        credential,
    })
}

fn origin(url: &Url) -> String {
    url.origin().ascii_serialization()
}

/// Loopback hosts are reached over plain HTTP when no endpoint says
/// otherwise, as pyxet did for `localhost` and `127.0.0.1`.
fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host == "[::1]"
        || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
}
