//! Integration tests for `braze-sync diff` (Catalog Schema).
//!
//! Each test stands up a wiremock server, writes a temporary
//! braze-sync.config.yaml plus an on-disk catalog schema, then invokes the
//! real binary via assert_cmd. Tests use `flavor = "multi_thread"` +
//! `spawn_blocking` for the same reason as cli_export.rs: assert_cmd's
//! sync wait would otherwise hold the only worker and starve wiremock.

mod common;

use assert_cmd::Command;
use common::{
    write_config, write_local_content_block, write_local_custom_attribute_registry,
    write_local_email_template, write_local_items, write_local_schema,
};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
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

// =====================================================================
// Content Block (v0.2.0)
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_content_block_orphan_when_local_missing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [
                {"content_block_id": "id-orphan", "name": "legacy_promo"}
            ]
        })))
        .mount(&server)
        .await;
    // No /info call: orphans don't need their body fetched.
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "content_block"])
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
    assert!(
        stdout.contains("Content Block: legacy_promo"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("orphaned"), "stdout: {stdout}");
    assert!(stdout.contains("1 orphan"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_content_block_added_when_remote_missing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"content_blocks": []})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "fresh", "Hello new\n");

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "content_block"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Content Block: fresh"), "stdout: {stdout}");
    assert!(stdout.contains("+ new content block"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_content_block_body_modified_shows_text_diff_summary() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-x", "name": "promo"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .and(query_param("content_block_id", "id-x"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "promo",
            "content": "line a\nold b\nline c\n",
            "tags": []
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "promo", "line a\nline b\nline c\n");

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "content_block"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("content changed (+1 -1)"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("1 changed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_content_block_no_drift_when_identical() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-stable", "name": "stable"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "stable",
            "content": "same body\n",
            "tags": []
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "stable", "same body\n");

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "content_block"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("no drift"), "stdout: {stdout}");
    assert!(stdout.contains("0 changed"), "stdout: {stdout}");
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

// =====================================================================
// Email Template (v0.3.0)
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_email_template_added_when_remote_missing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"templates": []})))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_email_template(tmp.path(), "fresh", "Welcome", "<p>Hi</p>", "Hi");

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "email_template"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("Email Template: fresh"), "stdout: {stdout}");
    assert!(stdout.contains("+ new email template"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_email_template_orphan_when_local_missing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "templates": [
                {"email_template_id": "id-old", "template_name": "old_campaign"}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    // No local email templates written

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "email_template"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("orphaned"), "stdout: {stdout}");
    assert!(stdout.contains("1 changed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_email_template_no_drift_when_identical() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "templates": [{"email_template_id": "id-w", "template_name": "welcome"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/templates/email/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "template_name": "welcome",
            "subject": "Hi",
            "body": "<p>Hi</p>",
            "plaintext_body": "Hi",
            "tags": [],
            "message": "success"
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_email_template(tmp.path(), "welcome", "Hi", "<p>Hi</p>", "Hi");

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "email_template"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("no drift"), "stdout: {stdout}");
    assert!(stdout.contains("0 changed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_catalog_items_detects_added_item() {
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
    Mock::given(method("GET"))
        .and(path("/catalogs/cardiology/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "message": "success"
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_items(tmp.path(), "cardiology", "id,name\naf001,atrial\n");
    let config_str = config_path.to_str().unwrap().to_string();

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", &config_str])
            .args(["diff", "--resource", "catalog_items"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("1 added"), "stdout: {stdout}");
    assert!(stdout.contains("1 changed"), "stdout: {stdout}");
}

// =====================================================================
// Custom Attribute (v0.5.0)
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_custom_attribute_deprecation_toggle() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "custom_attributes": [
                {
                    "custom_attribute_name": "legacy_field",
                    "data_type": "string",
                    "blocklisted": false
                }
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: legacy_field\n    type: string\n    deprecated: true\n",
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "custom_attribute"])
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
    assert!(
        stdout.contains("Custom Attribute: legacy_field"),
        "stdout: {stdout}"
    );
    assert!(stdout.contains("deprecated: false"), "stdout: {stdout}");
    assert!(stdout.contains("1 changed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_custom_attribute_unregistered_in_git() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "custom_attributes": [
                {"custom_attribute_name": "new_remote", "data_type": "string"}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(tmp.path(), "attributes: []\n");

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "custom_attribute"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("exists in Braze but not in Git"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_custom_attribute_no_drift_when_identical() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "custom_attributes": [
                {
                    "custom_attribute_name": "stable",
                    "data_type": "string",
                    "description": "A stable attribute",
                    "blocklisted": false
                }
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: stable\n    type: string\n    description: A stable attribute\n",
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "custom_attribute"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("no drift"), "stdout: {stdout}");
    assert!(stdout.contains("0 changed"), "stdout: {stdout}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_custom_attribute_no_local_file_all_unregistered() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
            "custom_attributes": [
                {"custom_attribute_name": "orphan_attr", "data_type": "string"}
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    // No registry file written — simulates fresh project

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "custom_attribute"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("exists in Braze but not in Git"),
        "stdout: {stdout}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_custom_attribute_name_filter_narrows_output() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 2,
            "custom_attributes": [
                {
                    "custom_attribute_name": "legacy_field",
                    "data_type": "string",
                    "blocklisted": false
                },
                {
                    "custom_attribute_name": "active_field",
                    "data_type": "string",
                    "blocklisted": false
                }
            ]
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: legacy_field\n    type: string\n    deprecated: true\n  \
         - name: active_field\n    type: string\n",
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args([
                "diff",
                "--resource",
                "custom_attribute",
                "--name",
                "legacy_field",
            ])
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
    // Only legacy_field should appear (filtered by --name).
    assert!(
        stdout.contains("Custom Attribute: legacy_field"),
        "stdout: {stdout}"
    );
    assert!(
        !stdout.contains("active_field"),
        "active_field should be filtered out; stdout: {stdout}"
    );
    assert!(stdout.contains("1 changed"), "stdout: {stdout}");
}
