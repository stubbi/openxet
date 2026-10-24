//! The one place routes are added to the CAS. Each route is built from its
//! list of methods and handlers, so what the router serves and what
//! `endpoints()` lists come from the same calls and can't drift.
//!
//! `clippy.toml` disallows adding routes to an axum `Router` directly, so a
//! route can't bypass [`Routes`] unnoticed.

use axum::Router;
use axum::handler::Handler;
use axum::http::Method;
use axum::routing::{MethodFilter, MethodRouter};

use crate::AppState;
use crate::cache;

/// One method and path template the CAS serves.
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
pub(crate) struct Handlers {
    router: MethodRouter<AppState>,
    #[cfg(feature = "test-support")]
    methods: Vec<Method>,
    private: bool,
}

/// Handles `method` with `handler`; add more methods with [`Handlers::on`].
pub(crate) fn on<H, T>(method: Method, handler: H) -> Handlers
where
    H: Handler<T, AppState>,
    T: 'static,
{
    Handlers {
        router: MethodRouter::new(),
        #[cfg(feature = "test-support")]
        methods: Vec::new(),
        private: false,
    }
    .on(method, handler)
}

impl Handlers {
    /// Also handles `method` with `handler`.
    ///
    /// # Panics
    ///
    /// If `method` is not one axum can route (`CONNECT`, or an extension
    /// method). Routes are built at startup, and every test builds them.
    pub(crate) fn on<H, T>(mut self, method: Method, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        let filter = MethodFilter::try_from(method.clone())
            .unwrap_or_else(|_| panic!("axum can't route {method}"));
        self.router = self.router.on(filter, handler);
        #[cfg(feature = "test-support")]
        self.methods.push(method);
        self
    }

    /// Marks every answer of the path, errors included, as depending on
    /// the caller's authorization ([`cache::private`]).
    pub(crate) fn private(mut self) -> Self {
        self.private = true;
        self
    }
}

/// Routes under construction, with the endpoints they serve.
pub(crate) struct Routes {
    router: Router<AppState>,
    #[cfg(feature = "test-support")]
    endpoints: Vec<Endpoint>,
}

#[expect(
    clippy::disallowed_methods,
    reason = "the builder every CAS route goes through, so it is listed"
)]
impl Routes {
    pub(crate) fn new() -> Self {
        Self {
            router: Router::new(),
            #[cfg(feature = "test-support")]
            endpoints: Vec::new(),
        }
    }

    /// Serves `methods` at `path`.
    pub(crate) fn route(mut self, path: &'static str, methods: Handlers) -> Self {
        #[cfg(feature = "test-support")]
        self.endpoints.extend(
            methods
                .methods
                .into_iter()
                .map(|method| Endpoint { method, path }),
        );
        let router = if methods.private {
            cache::private(methods.router)
        } else {
            methods.router
        };
        self.router = self.router.route(path, router);
        self
    }

    /// Adds every route of `other`.
    pub(crate) fn merge(mut self, other: Self) -> Self {
        self.router = self.router.merge(other.router);
        #[cfg(feature = "test-support")]
        self.endpoints.extend(other.endpoints);
        self
    }

    pub(crate) fn into_router(self) -> Router<AppState> {
        self.router
    }

    #[cfg(feature = "test-support")]
    pub(crate) fn into_endpoints(self) -> Vec<Endpoint> {
        self.endpoints
    }
}
