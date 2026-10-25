//! `GET /api/whoami-v2` (S3 fixture `01`, ruling M2-R3, ADR 0012 item 11).

use std::fmt::Write as _;

use axum::Json;
use axum::extract::State;
use axum::http::Method;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use crate::authz::{self, Who};
use crate::routes::{Routes, on};
use crate::{HubError, HubState};

/// The whoami route.
pub(crate) fn routes() -> Routes {
    Routes::new().route("/api/whoami-v2", on(Method::GET, whoami).private())
}

/// `GET /api/whoami-v2`: who the token acts for, in huggingface.co's shape.
///
/// `huggingface_hub` requires `name` (`_login.py` line 430, namespace
/// defaulting) and `orgs[].name` (`cli/auth.py` line 172), and reads
/// `auth.accessToken.displayName`, under which `hf auth login` stores the
/// token, `auth.accessToken.role` and `id` if present (S3). `name` is the
/// token's name or subject, `orgs` its `orgs` claim. `role` is
/// `fineGrained`: what the token may do is decided per repository by
/// `[[auth.rules]]`.
async fn whoami(State(state): State<HubState>, Who(who): Who) -> Result<Json<Value>, HubError> {
    if authz::is_anonymous(&who) {
        return Err(HubError::MissingCredential);
    }
    let orgs: Vec<Value> = who
        .claims
        .get("orgs")
        .into_iter()
        .flatten()
        .map(|org| {
            json!({
                "type": "org",
                "id": object_id("org", org),
                "name": org,
                "fullname": org,
                "avatarUrl": "",
            })
        })
        .collect();
    Ok(Json(json!({
        "type": "user",
        "id": object_id("user", &who.subject),
        "name": who.name,
        "fullname": who.name,
        "isPro": false,
        "avatarUrl": "",
        "orgs": orgs,
        "auth": {
            "type": "access_token",
            "accessToken": {
                "displayName": display_name(&state),
                "role": "fineGrained",
            },
        },
    })))
}

/// A stable 24-hex-digit id for `name` of `kind`, the form of
/// huggingface.co's object ids: the first 12 bytes of
/// SHA-256(`openxet-{kind}\n{name}`). The same subject always gets the
/// same id, and a user never shares one with an org.
fn object_id(kind: &str, name: &str) -> String {
    let digest = Sha256::digest(format!("openxet-{kind}\n{name}").as_bytes());
    digest[..12].iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    })
}

/// The name `hf auth login` stores the token under: `openxet-` and this
/// server's host, so tokens for different servers don't overwrite each
/// other. Only letters, digits, `.` and `-` are kept: the token store is an
/// INI file, where `:` and `=` separate keys from values.
fn display_name(state: &HubState) -> String {
    let host = state.cfg.public_url.host_str().unwrap_or("server");
    let host: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("openxet-{host}")
}
