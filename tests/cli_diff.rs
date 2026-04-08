//! Integration tests for `braze-sync diff` (Catalog Schema).
//!
//! Each test stands up a wiremock server, writes a temporary
//! braze-sync.config.yaml plus an on-disk catalog schema, then invokes the
//! real binary via assert_cmd. Tests use `flavor = "multi_thread"` +
//! `spawn_blocking` for the same reason as cli_export.rs: assert_cmd's
//! sync wait would otherwise hold the only worker and starve wiremock.

use assert_cmd::Command;
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_config(dir: &Path, server_uri: &str) -> PathBuf {
    let config_path = dir.join("braze-sync.config.yaml");
    let yaml = format!(
        "version: 1
default_environment: test
environments:
  test:
    api_endpoint: {server_uri}
    api_key_env: BRAZE_API_KEY
"
    );
    fs::write(&config_path, yaml).unwrap();
    config_path
}

fn write_local_schema(dir: &Path, name: &str, fields: &[(&str, &str)]) {
    let cat_dir = dir.join("catalogs").join(name);
    fs::create_dir_all(&cat_dir).unwrap();
    let mut yaml = format!("name: {name}\nfields:\n");
    for (n, t) in fields {
        yaml.push_str(&format!("  - name: {n}\n    type: {t}\n"));
    }
    fs::write(cat_dir.join("schema.yaml"), yaml).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_no_drift_when_local_matches_remote() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "stable", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "stable", &[("id", "string")]);

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("no drift"), "stdout: {stdout}");
    assert!(
        stdout.contains("Catalog Schema: stable"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("0 changed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_with_local_extra_field_shows_added_and_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "drift", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(
        tmp.path(),
        "drift",
        &[("id", "string"), ("extra", "number")],
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("+ field: extra"), "stdout: {stdout}");
    assert!(stdout.contains("1 changed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_fail_on_drift_with_drift_exits_two() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "drift", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(
        tmp.path(),
        "drift",
        &[("id", "string"), ("extra", "number")],
    );

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", "--fail-on-drift"])
            .assert()
            .failure()
            .code(2);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_fail_on_drift_no_drift_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "stable", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "stable", &[("id", "string")]);

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", "--fail-on-drift"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_with_json_format_emits_valid_v1_json() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "stable", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "stable", &[("id", "string")]);

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--format", "json"])
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let v: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("invalid json: {e}; got: {stdout}"));
    assert_eq!(v["version"], json!(1));
    assert_eq!(v["diffs"][0]["kind"], "catalog_schema");
    assert_eq!(v["diffs"][0]["name"], "stable");
    assert_eq!(v["diffs"][0]["op"], "unchanged");
}
