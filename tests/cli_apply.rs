//! Integration tests for `braze-sync apply` (Catalog Schema).
//!
//! Apply is the only command that mutates remote state, so the tests
//! lean heavily on wiremock's `.expect(0)` to assert "this write call
//! never happened" — that's how we prove the dry-run default and the
//! destructive guard actually keep their promises.
//!
//! 4 tests cover the 3 modes called out in IMPLEMENTATION.md A10:
//!
//! 1. dry-run (no `--confirm`)         → no write call, exit 0
//! 2. `--confirm` + non-destructive    → POST add field, exit 0
//! 3. `--confirm` + destructive (no `--allow-destructive`) → exit 6, no DELETE
//! 4. `--confirm --allow-destructive`  → DELETE field, exit 0

mod common;

use assert_cmd::Command;
use common::{write_config, write_local_schema};
use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_makes_no_write_calls_and_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "cardiology", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;
    // Any POST to wiremock will hit this mock; .expect(0) makes the test
    // panic on drop if the binary fired even one POST during dry-run.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(
        tmp.path(),
        "cardiology",
        &[("id", "string"), ("severity", "number")],
    );

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "catalog_schema"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_with_field_addition_calls_post_and_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "cardiology", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;
    // Pin the wire shape end-to-end: POST with the right body.
    Mock::given(method("POST"))
        .and(path("/catalogs/cardiology/fields"))
        .and(body_json(json!({
            "fields": [{"name": "severity", "type": "number"}]
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "ok"})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(
        tmp.path(),
        "cardiology",
        &[("id", "string"), ("severity", "number")],
    );

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "catalog_schema", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_with_destructive_change_without_allow_destructive_exits_6() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "cardiology", "fields": [
                    {"name": "id", "type": "string"},
                    {"name": "legacy", "type": "string"}
                ]}
            ]
        })))
        .mount(&server)
        .await;
    // Destructive guard MUST fire before the DELETE call.
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "cardiology", &[("id", "string")]);

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "catalog_schema", "--confirm"])
            .assert()
            .failure()
            .code(6);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_with_allow_destructive_calls_delete_and_exits_zero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "cardiology", "fields": [
                    {"name": "id", "type": "string"},
                    {"name": "legacy", "type": "string"}
                ]}
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/catalogs/cardiology/fields/legacy"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "cardiology", &[("id", "string")]);

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args([
                "apply",
                "--resource",
                "catalog_schema",
                "--confirm",
                "--allow-destructive",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}
