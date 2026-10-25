//! `openxet label` (ruling M2-R12), run as the real binary against a
//! dev-mode `openxet serve` through the labels API (`docs/reference/cli.md`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::process::Command;

use common::{Server, json_error, json_stdout, run};
use serde_json::json;
use tempfile::TempDir;

/// Creates `{dev}/{name}` through the Hub API, as `hf repo create` does.
fn create_repo(server: &Server, name: &str) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let status = runtime.block_on(async {
        reqwest::Client::new()
            .post(format!("{}/api/repos/create", server.endpoint))
            .bearer_auth(&server.token)
            .header("content-type", "application/json")
            .body(json!({ "name": name, "type": "model" }).to_string())
            .send()
            .await
            .unwrap()
            .status()
    });
    assert!(status.is_success(), "{status}");
}

fn label(server: &Server, dir: &TempDir) -> Command {
    let mut cmd = server.client(&server.token, &dir.path().join("cache"));
    cmd.arg("label");
    cmd
}

#[test]
fn label_round_trip_json() {
    let dir = TempDir::new().unwrap();
    let server = Server::start(&dir.path().join("data"), None);
    create_repo(&server, "m");
    let labels = |args: &[&str]| json_stdout(&run(label(&server, &dir).args(args)));

    assert_eq!(
        labels(&["list", "dev/m", "--json"]),
        json!({"schema": "openxet.labels.v1", "repo": "dev/m", "labels": ["model"]})
    );
    // A dry run answers what the change would make, and changes nothing.
    assert_eq!(
        labels(&["add", "dev/m", "eval-logs", "--dry-run", "--json"]),
        json!({
            "schema": "openxet.labels.v1",
            "repo": "dev/m",
            "labels": ["model", "eval-logs"],
            "dry_run": true,
        })
    );
    assert_eq!(
        labels(&["remove", "dev/m", "model", "--dry-run", "--json"])["labels"],
        json!([])
    );
    assert_eq!(
        labels(&["list", "dev/m", "--json"])["labels"],
        json!(["model"])
    );

    assert_eq!(
        labels(&["add", "dev/m", "eval-logs", "--json"]),
        json!({
            "schema": "openxet.labels.v1",
            "repo": "dev/m",
            "labels": ["model", "eval-logs"],
            "dry_run": false,
        })
    );
    // Adding a label it carries, or removing one it doesn't, changes nothing
    // and succeeds, so a retry is safe.
    assert_eq!(
        labels(&["add", "dev/m", "eval-logs", "--json"])["labels"],
        json!(["model", "eval-logs"])
    );
    assert_eq!(
        labels(&["remove", "dev/m", "model", "--json"])["labels"],
        json!(["eval-logs"])
    );
    assert_eq!(
        labels(&["remove", "dev/m", "model", "--json"])["labels"],
        json!(["eval-logs"])
    );
    // The name is matched ignoring case and answered as stored.
    assert_eq!(
        labels(&["list", "DEV/M", "--json"]),
        json!({"schema": "openxet.labels.v1", "repo": "dev/m", "labels": ["eval-logs"]})
    );

    let human = run(label(&server, &dir).args(["list", "dev/m"]));
    assert!(human.status.success());
    assert_eq!(
        String::from_utf8(human.stdout).unwrap(),
        "dev/m: eval-logs\n"
    );
}

#[test]
fn label_errors_are_structured_with_hints() {
    let dir = TempDir::new().unwrap();
    let server = Server::start(&dir.path().join("data"), None);
    create_repo(&server, "m");
    let error = |cmd: &mut Command, exit: i32, code: &str| {
        let error = json_error(&run(cmd), exit);
        assert_eq!(error["code"], code, "{error}");
        error
    };

    // The server's unknown-repository answer, with its hint.
    let missing = error(
        label(&server, &dir).args(["list", "dev/nope", "--json"]),
        1,
        "repo_not_found",
    );
    assert_eq!(missing["message"], "Repository not found");
    error(
        label(&server, &dir).args(["add", "dev/nope", "x", "--json"]),
        1,
        "repo_not_found",
    );

    // Arguments the server would refuse are refused before any request.
    let bad = error(
        label(&server, &dir).args(["add", "dev/m", "Not_A_Slug", "--json"]),
        2,
        "invalid_label",
    );
    assert!(bad["hint"].as_str().unwrap().contains("slug"), "{bad}");
    error(
        label(&server, &dir).args(["list", "no-slash", "--json"]),
        2,
        "invalid_repo",
    );
    error(
        label(&server, &dir).args(["list", "dev/bad..name", "--json"]),
        2,
        "invalid_repo",
    );

    // Credentials.
    let unauthorized = error(
        label(&server, &dir)
            .args(["list", "dev/m", "--json"])
            .env("OPENXET_TOKEN", "REDACTED"),
        1,
        "unauthorized",
    );
    assert!(
        unauthorized["hint"]
            .as_str()
            .unwrap()
            .contains("OPENXET_TOKEN"),
        "{unauthorized}"
    );
    error(
        label(&server, &dir)
            .args(["list", "dev/m", "--json"])
            .env_remove("OPENXET_TOKEN"),
        2,
        "missing_token",
    );
    error(
        label(&server, &dir)
            .args(["list", "dev/m", "--json"])
            .env_remove("OPENXET_ENDPOINT"),
        2,
        "missing_endpoint",
    );
    error(
        label(&server, &dir).args([
            "list",
            "dev/m",
            "--json",
            "--endpoint",
            "http://127.0.0.1:0",
        ]),
        1,
        "unreachable_endpoint",
    );

    // Without --json, the same error as two lines.
    let human = run(label(&server, &dir).args(["list", "dev/nope"]));
    assert_eq!(human.status.code(), Some(1));
    let text = String::from_utf8(human.stderr).unwrap();
    assert!(
        text.starts_with("error: Repository not found\nhint: "),
        "{text}"
    );
}
