//! `openxet openapi` and `GET /openapi.json`: the server's OpenAPI 3.1
//! document, generated from the routes it serves (ruling M2-R15). Both
//! give the same bytes, and `docs/reference/openapi.json` is a copy that
//! `scripts/check-openapi.sh` keeps current.

use std::sync::LazyLock;

use axum::http::header::CONTENT_TYPE;
use axum::http::{Method, StatusCode};
use openxet_http::{Area, Credential, Doc, Mounted, Routes, on};
use serde::Serialize;

use crate::error::CliError;
use crate::output::{HumanDisplay, Output};

/// The document, rendered once: the routes are fixed at build time.
static DOCUMENT: LazyLock<String> = LazyLock::new(render);

fn render() -> String {
    let doc = openxet_http::document(
        "OpenXet",
        env!("CARGO_PKG_VERSION"),
        [
            openxet_cas::routes().into_paths(),
            openxet_hub::routes().into_paths(),
            routes().into_paths(),
        ],
    );
    let mut text = doc
        .to_pretty_json()
        .unwrap_or_else(|e| unreachable!("the document is plain JSON values: {e}"));
    text.push('\n');
    text
}

/// `GET /openapi.json`. It needs no credential: it describes routes and
/// never data.
fn routes() -> Routes<()> {
    Routes::new().route(
        "/openapi.json",
        on(
            Method::GET,
            Doc::new(Area::Meta, "This document", Credential::None)
                .describe("What `openxet openapi` prints, byte for byte.")
                .answer(
                    StatusCode::OK,
                    "The OpenAPI 3.1 document.",
                    Some(("application/json", serde_json::json!({"type": "object"}))),
                ),
            || async { ([(CONTENT_TYPE, "application/json")], DOCUMENT.as_str()) },
        ),
    )
}

/// The `/openapi.json` route, for [`openxet_http::compose`].
pub fn mounted() -> Mounted {
    routes().with_state(())
}

/// What `openxet openapi` prints: the document as it is, or with
/// `--json` inside one object with a versioned `schema`, as every
/// command's `--json` is.
#[derive(Serialize)]
struct Printed {
    schema: &'static str,
    document: serde_json::Value,
}

impl HumanDisplay for Printed {
    fn human(&self) -> String {
        DOCUMENT.trim_end_matches('\n').to_owned()
    }
}

/// `openxet openapi`: prints the document.
pub fn run(output: Output) -> Result<(), CliError> {
    let document = match output {
        Output::Human => serde_json::Value::Null,
        Output::Json => serde_json::from_str(&DOCUMENT)
            .unwrap_or_else(|e| unreachable!("the document is the JSON it rendered: {e}")),
    };
    output
        .emit(&Printed {
            schema: "openxet.openapi.v1",
            document,
        })
        .map_err(|err| CliError::output(&err))
}
