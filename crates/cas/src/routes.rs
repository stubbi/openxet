//! The CAS's routes are built with `openxet_http`'s route builder, the one
//! way to add a route (ruling M2-R1), over the CAS's [`AppState`].

use crate::AppState;

pub(crate) use openxet_http::on;

/// CAS routes under construction.
pub(crate) type Routes = openxet_http::Routes<AppState>;
