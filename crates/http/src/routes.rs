//! The one place routes are added to the server. Each route is built from
//! its list of methods, handlers and their [`Doc`]s, so what a router
//! serves, what `endpoints()` lists and what the OpenAPI document describes
//! come from the same calls and can't drift (ruling M2-R15).
//!
//! `clippy.toml` disallows adding routes or fallbacks to an axum `Router`
//! directly, so a route can't bypass [`Routes`] unnoticed and no second
//! fallback slips in beside [`crate::compose`]'s.

use axum::Router;
use axum::handler::Handler;
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderValue, Method};
use axum::response::Response;
use axum::routing::{MethodFilter, MethodRouter};
use utoipa::openapi::Paths;

use crate::doc::{self, Doc};
use crate::error;

/// One method and path template a router serves.
///
/// A route with a `GET` handler and no `HEAD` handler also answers `HEAD`:
/// axum runs the `GET` handler, extractors and credential check included,
/// and drops the body. Such a `HEAD` is not listed separately.
#[cfg(feature = "test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The method a handler is registered for.
    pub method: Method,
    /// The path template, with `{name}` for each path parameter.
    pub path: &'static str,
}

/// The handlers of one path, at least one, each for one method.
pub struct Handlers<S> {
    router: MethodRouter<S>,
    docs: Vec<(Method, Doc)>,
    private: bool,
}

/// Handles `method` with `handler`, described by `doc`; add more methods
/// with [`Handlers::on`].
///
/// # Panics
///
/// As [`Handlers::on`].
pub fn on<S, H, T>(method: Method, doc: Doc, handler: H) -> Handlers<S>
where
    S: Clone + Send + Sync + 'static,
    H: Handler<T, S>,
    T: 'static,
{
    Handlers {
        router: MethodRouter::new(),
        docs: Vec::new(),
        private: false,
    }
    .on(method, doc, handler)
}

impl<S> Handlers<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// Also handles `method` with `handler`, described by `doc`.
    ///
    /// # Panics
    ///
    /// If `method` is not one axum can route (`CONNECT`, or an extension
    /// method). Routes are built at startup, and every test builds them.
    #[must_use]
    pub fn on<H, T>(mut self, method: Method, doc: Doc, handler: H) -> Self
    where
        H: Handler<T, S>,
        T: 'static,
    {
        let filter = MethodFilter::try_from(method.clone())
            .unwrap_or_else(|_| panic!("axum can't route {method}"));
        self.router = self.router.on(filter, handler);
        self.docs.push((method, doc));
        self
    }

    /// Marks every answer of the path, errors included, as depending on
    /// the caller's authorization: `Cache-Control: no-store`.
    #[must_use]
    pub fn private(mut self) -> Self {
        self.private = true;
        self
    }
}

/// Routes under construction, with the endpoints they serve and their
/// description.
pub struct Routes<S> {
    router: Router<S>,
    /// Each route's path, method, description and whether it is private,
    /// turned into the document only by [`Routes::into_paths`], so building
    /// a router costs no more than its descriptions' values.
    docs: Vec<(&'static str, Method, Doc, bool)>,
    #[cfg(feature = "test-support")]
    endpoints: Vec<Endpoint>,
}

impl<S> Default for Routes<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the builder every route goes through, so it is listed"
)]
impl<S> Routes<S>
where
    S: Clone + Send + Sync + 'static,
{
    /// No routes yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            docs: Vec::new(),
            #[cfg(feature = "test-support")]
            endpoints: Vec::new(),
        }
    }

    /// Serves `methods` at `path`.
    ///
    /// # Panics
    ///
    /// If `path` is already served or is not a valid axum path, as
    /// [`axum::Router::route`]. Routes are built at startup, and every test
    /// builds them.
    #[must_use]
    pub fn route(mut self, path: &'static str, methods: Handlers<S>) -> Self {
        for (method, doc) in methods.docs {
            #[cfg(feature = "test-support")]
            self.endpoints.push(Endpoint {
                method: method.clone(),
                path,
            });
            self.docs.push((path, method, doc, methods.private));
        }
        let router = if methods.private {
            private(methods.router)
        } else {
            methods.router
        };
        self.router = self.router.route(path, router);
        self
    }

    /// Adds every route of `other`.
    ///
    /// # Panics
    ///
    /// If both serve a path, as [`axum::Router::merge`].
    #[must_use]
    pub fn merge(mut self, other: Self) -> Self {
        self.router = self.router.merge(other.router);
        self.docs.extend(other.docs);
        #[cfg(feature = "test-support")]
        self.endpoints.extend(other.endpoints);
        self
    }

    /// The routes with their state, ready for [`crate::compose`].
    pub fn with_state(self, state: S) -> Mounted {
        Mounted {
            router: self.router.with_state(state),
            #[cfg(feature = "test-support")]
            endpoints: self.endpoints,
        }
    }

    /// The description of every route, for [`crate::document`].
    ///
    /// # Panics
    ///
    /// If a description holds a schema the document's model can't carry
    /// as written. The document's tests render every route.
    #[must_use]
    pub fn into_paths(self) -> Paths {
        let mut paths = Paths::new();
        for (path, method, doc, private) in self.docs {
            paths.add_path_operation(
                doc::openapi_path(path),
                vec![doc::http_method(&method)],
                doc.operation(&method, path, private),
            );
        }
        paths
    }

    /// Every method and path template these routes serve. Only with the
    /// `test-support` feature.
    #[cfg(feature = "test-support")]
    #[must_use]
    pub fn into_endpoints(self) -> Vec<Endpoint> {
        self.endpoints
    }
}

/// One part of the server (the CAS, the Hub, Git) with its state: routes
/// without a fallback or middleware, which [`crate::compose`] applies once
/// to every part together.
pub struct Mounted {
    pub(crate) router: Router,
    #[cfg(feature = "test-support")]
    pub(crate) endpoints: Vec<Endpoint>,
}

/// `route` with the `405` fallback, and `Cache-Control: no-store` on every
/// answer, the `405` included. The fallback is the method router's own so
/// that the layer covers it; axum still adds the `Allow` header. Every
/// answer of a route that depends on the caller's authorization carries
/// it, errors included, so no cache or proxy can serve one caller's answer
/// to another (design §5.9 req 3).
fn private<S>(route: MethodRouter<S>) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    route
        .fallback(error::method_not_allowed)
        .layer(axum::middleware::map_response(no_store))
}

async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
