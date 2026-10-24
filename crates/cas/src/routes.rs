//! The one place routes are added to the CAS, so the router and the list
//! of what it serves ([`crate::endpoints`]) come from the same calls and
//! can't drift.

use axum::Router;
use axum::http::Method;
use axum::routing::MethodRouter;

use crate::AppState;

/// One method and path template the CAS serves.
///
/// A route with a `GET` handler and no `HEAD` handler also answers `HEAD`:
/// axum runs the `GET` handler, extractors and credential check included,
/// and drops the body. Such a `HEAD` is not listed separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The method a handler is registered for.
    pub method: Method,
    /// The path template, with `{name}` for each path parameter.
    pub path: &'static str,
}

/// Routes under construction, with the endpoints they serve.
pub(crate) struct Routes {
    router: Router<AppState>,
    endpoints: Vec<Endpoint>,
}

impl Routes {
    pub(crate) fn new() -> Self {
        Self {
            router: Router::new(),
            endpoints: Vec::new(),
        }
    }

    /// Serves `handlers` at `path`. `methods` are the methods `handlers`
    /// has a handler for; `router_serves_exactly_its_endpoints` in
    /// `tests/routes_misc.rs` probes the router to keep the two equal.
    pub(crate) fn route(
        mut self,
        path: &'static str,
        methods: &[Method],
        handlers: MethodRouter<AppState>,
    ) -> Self {
        self.router = self.router.route(path, handlers);
        self.endpoints.extend(methods.iter().map(|method| Endpoint {
            method: method.clone(),
            path,
        }));
        self
    }

    /// Adds every route of `other`.
    pub(crate) fn merge(mut self, other: Self) -> Self {
        self.router = self.router.merge(other.router);
        self.endpoints.extend(other.endpoints);
        self
    }

    pub(crate) fn into_router(self) -> Router<AppState> {
        self.router
    }

    pub(crate) fn into_endpoints(self) -> Vec<Endpoint> {
        self.endpoints
    }
}
