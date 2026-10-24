//! Caching headers for answers that depend on the caller's credential.

use axum::http::HeaderValue;
use axum::http::header::CACHE_CONTROL;
use axum::response::Response;
use axum::routing::MethodRouter;

use crate::{AppState, error};

/// `route` with the `405` fallback, and [`no_store`] on every answer, the
/// `405` included. The fallback is the method router's own so that the
/// layer covers it; axum still adds the `Allow` header.
pub(crate) fn private(route: MethodRouter<AppState>) -> MethodRouter<AppState> {
    route
        .fallback(error::method_not_allowed)
        .layer(axum::middleware::map_response(no_store))
}

/// Marks `response` `Cache-Control: no-store`. Every answer of a route that
/// depends on the caller's authorization carries it, errors included, so no
/// cache or proxy can serve one caller's answer to another (design §5.9
/// req 3).
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
