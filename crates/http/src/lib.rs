//! openxet's HTTP plumbing, shared by the CAS, Hub and Git parts of the
//! server (ruling M2-R1).
//!
//! Each part builds its routes with [`Routes`], the one way to add a route,
//! and mounts them with its own state ([`Routes::with_state`]); `openxet
//! serve` then joins the parts with [`compose`], which applies one fallback
//! and one middleware stack to them all (issue #46).
//!
//! Every response, success or error, carries an `X-Request-Id`
//! ([`X_REQUEST_ID`]) that is also a field of the request's tracing span, so
//! a failure a client logs ties back to the server's log. Every error body is
//! `{"schema":"openxet.error.v1","code","message","hint"}` ([`ErrorBody`]).

#![forbid(unsafe_code)]

mod clock;
mod error;
mod middleware;
mod request_id;
mod routes;

pub use clock::{Clock, SystemClock};
pub use error::{ErrorBody, HubErrorBody, SCHEMA, X_ERROR_CODE, X_ERROR_MESSAGE};
#[cfg(feature = "test-support")]
pub use middleware::endpoints;
pub use middleware::{BodyDeadline, BodyTimedOut, compose, timed_out};
pub use request_id::{RequestId, X_REQUEST_ID, assign, span};
#[cfg(feature = "test-support")]
pub use routes::Endpoint;
pub use routes::{Handlers, Mounted, Routes, on};
