use assert_cmd::{Command, cargo::cargo_bin_cmd};

fn openxet() -> Command {
    cargo_bin_cmd!("openxet")
}

#[test]
fn version_human_names_the_binary_and_semver() {
    openxet()
        .arg("version")
        .assert()
        .success()
        .stdout(predicates::str::starts_with(format!(
            "openxet {}",
            env!("CARGO_PKG_VERSION")
        )));
}

#[test]
fn version_json_is_versioned_schema() {
    let out = openxet().args(["version", "--json"]).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema"], "openxet.version.v1");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["git_commit"].is_string());
    assert!(v["protocol_spec"].as_str().unwrap().starts_with("1.1"));
}

#[test]
fn unknown_command_exits_with_usage_code_and_json_error_when_asked() {
    let out = openxet().args(["nope", "--json"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(v["schema"], "openxet.error.v1");
    assert_eq!(v["code"], "usage");
    assert!(v["hint"].as_str().unwrap().contains("openxet --help"));
}

#[test]
fn unknown_command_without_json_prints_clap_usage_error() {
    let out = openxet().arg("nope").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope"));
}
