//! Integration tests for `braze-sync diff` (Catalog Schema).
//!
//! Each test stands up a wiremock server, writes a temporary
//! braze-sync.config.yaml plus an on-disk catalog schema, then invokes the
//! real binary via assert_cmd. Tests use `flavor = "multi_thread"` +
//! `spawn_blocking` for the same reason as cli_export.rs: assert_cmd's
//! sync wait would otherwise hold the only worker and starve wiremock.

mod common;

use assert_cmd::Command;
use common::{write_config, write_local_schema};
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
