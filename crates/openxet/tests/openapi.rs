//! The OpenAPI document (ruling M2-R15): `openxet openapi` describes every
//! route the server serves, is valid OpenAPI 3.1, and is what `GET
//! /openapi.json` serves.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeSet, HashSet};

use common::{Server, openxet, run};
use serde_json::Value;
use tempfile::TempDir;

/// The document `openxet openapi` prints, as printed.
fn printed() -> String {
    let out = run(openxet().arg("openapi"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn document() -> Value {
    serde_json::from_str(&printed()).unwrap()
}

/// Every method and path the server serves, in the document's spelling:
/// lowercase methods, and a catch-all `{*rest}` as `{rest}`.
fn served() -> BTreeSet<(String, String)> {
    openxet_cas::endpoints()
        .into_iter()
        .chain(openxet_hub::endpoints())
        .map(|e| {
            (
                e.method.as_str().to_ascii_lowercase(),
                e.path.replace("{*", "{"),
            )
        })
        .chain([("get".to_owned(), "/openapi.json".to_owned())])
        .collect()
}

const METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

/// Every operation of `doc`: method, path, operation.
fn operations(doc: &Value) -> Vec<(String, String, &Value)> {
    let mut ops = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in METHODS {
            if let Some(op) = item.get(method) {
                ops.push((method.to_owned(), path.clone(), op));
            }
        }
    }
    ops
}

/// `reference` resolved in `doc`, if it is a `$ref`.
fn resolve<'a>(doc: &'a Value, value: &'a Value) -> &'a Value {
    match value.get("$ref").and_then(Value::as_str) {
        Some(reference) => doc
            .pointer(reference.strip_prefix('#').unwrap())
            .unwrap_or_else(|| panic!("{reference} resolves to nothing")),
        None => value,
    }
}

#[test]
fn openapi_lists_every_served_route() {
    let doc = document();
    let ops = operations(&doc);
    let described: BTreeSet<(String, String)> =
        ops.iter().map(|(m, p, _)| (m.clone(), p.clone())).collect();
    let served = served();
    let missing: Vec<_> = served.difference(&described).collect();
    let extra: Vec<_> = described.difference(&served).collect();
    assert!(missing.is_empty(), "served but not described: {missing:?}");
    assert!(extra.is_empty(), "described but not served: {extra:?}");

    let error_bodies = [
        "#/components/schemas/ErrorBody",
        "#/components/schemas/HubErrorBody",
        "#/components/schemas/GitErrorText",
    ];
    let mut ids = HashSet::new();
    for (method, path, op) in &ops {
        let at = format!("{method} {path}");
        assert!(
            op["summary"].as_str().is_some_and(|s| !s.is_empty()),
            "{at}: no summary"
        );
        assert!(
            ids.insert(op["operationId"].as_str().unwrap().to_owned()),
            "{at}: operationId is not unique"
        );

        // Each template parameter is a required path parameter.
        let params: Vec<&Value> = op["parameters"]
            .as_array()
            .map(|p| p.iter().collect())
            .unwrap_or_default();
        for name in path
            .split('/')
            .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
        {
            assert!(
                params.iter().any(|p| p["in"] == "path"
                    && p["name"] == name
                    && p["required"] == true
                    && p["description"].as_str().is_some_and(|d| !d.is_empty())),
                "{at}: path parameter {name} is not declared with a description"
            );
        }

        // A route that only refuses (a Hub endpoint OpenXet doesn't
        // implement, `HEAD` of a transfer URL) has no successful answer.
        let responses = op["responses"].as_object().unwrap();
        assert!(!responses.is_empty(), "{at}: no answer");
        for (status, response) in responses {
            let response = resolve(&doc, response);
            assert!(
                response["description"]
                    .as_str()
                    .is_some_and(|d| !d.is_empty()),
                "{at} {status}: no description"
            );
            assert!(
                response["headers"].get("X-Request-Id").is_some(),
                "{at} {status}: no X-Request-Id"
            );
            let content = response["content"].as_object();
            if status == "416" && content.is_none() {
                // A file read's `416` carries only `Content-Range`, as HTTP
                // defines it (RFC 9110 section 15.5.17) and plain HTTP
                // clients expect.
                assert!(
                    response["headers"].get("Content-Range").is_some(),
                    "{at} 416"
                );
            } else if status.starts_with('4') || status.starts_with('5') {
                let content = content.unwrap_or_else(|| panic!("{at} {status}: no error body"));
                assert!(!content.is_empty(), "{at} {status}: no error body");
                for (media_type, media) in content {
                    let schema = &media["schema"];
                    assert!(
                        error_bodies.contains(&schema["$ref"].as_str().unwrap_or_default()),
                        "{at} {status} {media_type}: the error body is not referenced: {schema}"
                    );
                }
            } else {
                for (media_type, media) in content.into_iter().flatten() {
                    assert!(
                        media.get("schema").is_some(),
                        "{at} {status} {media_type}: no schema"
                    );
                }
            }
        }
    }
}

#[test]
fn openapi_is_valid_3_1() {
    let text = printed();
    let doc: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(doc["openapi"], "3.1.0");

    // utoipa's own model reads it back without losing anything.
    let model: utoipa::openapi::OpenApi = serde_json::from_str(&text).unwrap();
    assert_eq!(serde_json::to_value(&model).unwrap(), doc);

    let schema: Value = serde_json::from_str(include_str!(
        "../testdata/openapi-3.1-schema-2026-08-03.json"
    ))
    .unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let errors: Vec<String> = validator
        .iter_errors(&doc)
        .map(|e| format!("{}: {e}", e.instance_path()))
        .collect();
    assert!(
        errors.is_empty(),
        "not valid OpenAPI 3.1:\n{}",
        errors.join("\n")
    );
    // The check has teeth: a document without `info` is refused.
    let mut broken = doc.clone();
    broken.as_object_mut().unwrap().remove("info");
    assert!(!validator.is_valid(&broken));

    // Every `$ref` resolves.
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    for reference in refs {
        assert!(
            doc.pointer(reference.strip_prefix('#').unwrap()).is_some(),
            "{reference} resolves to nothing"
        );
    }
}

fn collect_refs(value: &Value, refs: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(Value::String(reference)) = map.get("$ref") {
                refs.push(reference.clone());
            }
            map.values().for_each(|v| collect_refs(v, refs));
        }
        Value::Array(items) => items.iter().for_each(|v| collect_refs(v, refs)),
        _ => {}
    }
}

#[test]
fn openapi_names_no_credential_value() {
    // A credential is described by where it goes, never by a value, and
    // nothing in the document is an example a reader could mistake for one.
    let text = printed();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let mut keys = Vec::new();
    collect_keys(&doc, &mut keys);
    assert!(!keys.iter().any(|k| k == "example" || k == "examples"));
    // Spelled with `concat!` so the repository's credential scan doesn't
    // read the markers as credentials.
    for marker in [
        concat!("Bearer", " "),
        concat!("Basic", " "),
        concat!("sig", "="),
        concat!("Signature", "="),
    ] {
        for (at, _) in text.match_indices(marker) {
            let value: String = text[at + marker.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || "._~+/=-".contains(*c))
                .collect();
            // A challenge's `realm=` names no credential.
            assert!(
                value.is_empty() || value == "REDACTED" || value == "realm=",
                "{marker}{value} in the document"
            );
        }
    }
}

fn collect_keys(value: &Value, keys: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                keys.push(key.clone());
                collect_keys(v, keys);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_keys(v, keys)),
        _ => {}
    }
}

#[test]
fn json_flag_wraps_the_document_in_a_versioned_object() {
    // Every command's `--json` is one object with a versioned `schema`.
    let out = run(openxet().args(["openapi", "--json"]));
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(text.lines().count(), 1);
    let wrapped: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(wrapped["schema"], "openxet.openapi.v1");
    assert_eq!(wrapped["document"], document());
}

#[test]
fn openapi_json_serves_the_printed_document() {
    let printed = printed();
    let dir = TempDir::new().unwrap();
    let server = Server::start(dir.path(), None);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (status, cache, content_type, body) = runtime.block_on(async {
        // No credential: the document describes routes, never data.
        let response = reqwest::get(format!("{}/openapi.json", server.endpoint))
            .await
            .unwrap();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .map(|v| v.to_str().unwrap().to_owned())
        };
        let (cache, content_type) = (header("cache-control"), header("content-type"));
        (
            response.status(),
            cache,
            content_type,
            response.text().await.unwrap(),
        )
    });
    assert_eq!(status, 200);
    assert_eq!(content_type.as_deref(), Some("application/json"));
    assert_eq!(cache, None);
    assert!(
        body == printed,
        "GET /openapi.json differs from `openxet openapi`"
    );
    server.stop();
}
