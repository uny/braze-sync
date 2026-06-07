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
use common::{
    write_config, write_local_content_block, write_local_custom_attribute_registry,
    write_local_email_template, write_local_schema,
};
use predicates::prelude::*;
use serde_json::json;
use wiremock::matchers::{body_json, body_partial_json, method, path, query_param};
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
async fn dry_run_with_json_format_emits_valid_v1_json() {
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
    Mock::given(method("POST"))
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

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--format", "json"])
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "catalog_schema"])
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
    assert_eq!(v["summary"]["changed"], json!(1));
    assert_eq!(v["diffs"][0]["kind"], "catalog_schema");
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

/// Local-side absence of a catalog must produce a top-level DELETE
/// against `/catalogs/{name}` and skip the per-field DELETE loop — one
/// remote call drops the schema and all items.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_with_allow_destructive_deletes_entire_catalog_in_one_call() {
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
        .and(path("/catalogs/cardiology"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    // Per-field DELETEs must NOT fire when the whole catalog is dropped.
    Mock::given(method("DELETE"))
        .and(path("/catalogs/cardiology/fields/legacy"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    // No local schema for "cardiology" — diff produces top-level Removed.

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

/// Top-level catalog removal is destructive and must be gated by
/// `--allow-destructive`; without it, apply exits 6 and issues no
/// DELETE.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn confirm_without_allow_destructive_blocks_full_catalog_delete() {
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
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    // No local schema for "cardiology".

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
async fn confirm_with_new_catalog_posts_create_and_skips_per_field_post() {
    let server = MockServer::start().await;
    // Remote has no catalogs — local introduces "cardiology".
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/catalogs"))
        .and(body_json(json!({
            "catalogs": [{
                "name": "cardiology",
                "fields": [
                    {"name": "id", "type": "string"},
                    {"name": "severity", "type": "number"}
                ]
            }]
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"message": "ok"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/catalogs/cardiology/fields"))
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
            .args(["apply", "--resource", "catalog_schema", "--confirm"])
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
async fn content_block_dry_run_makes_no_write_calls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "fresh", "Hello\n");

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_confirm_create_posts_to_create_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .and(body_json(json!({
            "name": "fresh",
            "content": "Hello\n",
            "tags": [],
            "state": "active"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_block_id": "new-id-1",
            "message": "success"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "fresh", "Hello\n");

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_confirm_update_posts_to_update_endpoint_with_id() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-promo", "name": "promo"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .and(query_param("content_block_id", "id-promo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "promo",
            "content": "old body\n",
            "tags": []
        })))
        .mount(&server)
        .await;
    // Pins the update body. `state` is deliberately absent — see
    // `braze::content_block::update_content_block` for the rationale
    // (state is local-only per the README and must not be sent on
    // updates, where it could silently overwrite remote state that
    // braze-sync cannot observe via /info).
    Mock::given(method("POST"))
        .and(path("/content_blocks/update"))
        .and(body_json(json!({
            "content_block_id": "id-promo",
            "name": "promo",
            "content": "new body\n",
            "tags": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "promo", "new body\n");

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_orphan_makes_no_write_calls() {
    // Orphan policy is report-only: content block orphans never trigger
    // a write, even with --confirm + --allow-destructive. Braze rejects
    // renaming a content block after activation and exposes no DELETE
    // endpoint, so there is nothing apply can safely do — the orphan is
    // surfaced in the diff report instead. (Content Block has no
    // destructive ops, so --allow-destructive is operationally a no-op
    // here; passing it pins that it cannot trigger a remote mutation.)
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-orphan", "name": "legacy"}]
        })))
        .mount(&server)
        .await;
    // /info must NOT be fetched: with no archive flag we never need
    // the body, and a stray fetch would suggest the orphan path is
    // doing more than the §11.6 report-only contract allows.
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    // No local file for "legacy" → orphan from braze-sync's POV.

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args([
                "apply",
                "--resource",
                "content_block",
                "--confirm",
                "--allow-destructive",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_orphan_is_reported() {
    // Orphans are surfaced as an always-on, read-only report: the orphan
    // name and the dashboard/exclude_patterns guidance appear in the diff
    // output, while the apply path makes ZERO write calls (no /info fetch,
    // no /update). See docs/orphan-tracking.md.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-orphan", "name": "legacy"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .assert()
            .success()
            // The orphan is surfaced for manual handling rather than archived.
            .stdout(predicate::str::contains("legacy"))
            .stdout(predicate::str::contains("not present in Git"));
    })
    .await
    .unwrap();
}

// =====================================================================
// Email Template (v0.3.0)
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_template_dry_run_makes_no_write_calls() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"templates": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/templates/email/create"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_email_template(tmp.path(), "fresh", "Welcome", "<p>Hi</p>", "Hi");

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "email_template"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_template_confirm_create_posts_to_create_endpoint() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"templates": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/templates/email/create"))
        .and(body_json(json!({
            "template_name": "fresh",
            "subject": "Welcome",
            "body": "<p>Hi</p>",
            "plaintext_body": "Hi",
            "tags": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "email_template_id": "new-id"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_email_template(tmp.path(), "fresh", "Welcome", "<p>Hi</p>", "Hi");

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "email_template", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_template_confirm_update_posts_to_update_endpoint() {
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
        .and(query_param("email_template_id", "id-w"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "template_name": "welcome",
            "subject": "Old subject",
            "body": "<p>Old</p>",
            "plaintext_body": "Old",
            "tags": [],
            "message": "success"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/templates/email/update"))
        .and(body_json(json!({
            "email_template_id": "id-w",
            "template_name": "welcome",
            "subject": "New subject",
            "body": "<p>New</p>",
            "plaintext_body": "New",
            "tags": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_email_template(tmp.path(), "welcome", "New subject", "<p>New</p>", "New");

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "email_template", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_template_orphan_makes_no_write_calls() {
    // Email template orphans are report-only: Braze exposes no DELETE and
    // no usage data, and renaming an API-referenced template breaks
    // send-time resolution, so apply makes ZERO write calls (no /info
    // fetch, no /update). The orphan is surfaced in the diff report.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "templates": [{"email_template_id": "id-old", "template_name": "old_promo"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/templates/email/info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    // No local file for "old_promo" → orphan from braze-sync's POV.

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "email_template", "--confirm"])
            .assert()
            .success()
            .stdout(predicate::str::contains("old_promo"))
            .stdout(predicate::str::contains("not present in Git"));
    })
    .await
    .unwrap();
}

// =====================================================================
// Custom Attribute
// =====================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_custom_attribute_deprecation_toggle_with_confirm() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": [
                {
                    "name": "legacy_field",
                    "data_type": "string",
                    "status": "Active"
                }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/custom_attributes/blocklist"))
        .and(body_json(json!({
            "custom_attribute_names": ["legacy_field"],
            "blocklisted": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: legacy_field\n    type: string\n    deprecated: true\n",
    );

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "custom_attribute", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_custom_attribute_dry_run_makes_no_write_call() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": [
                {
                    "name": "legacy_field",
                    "data_type": "string",
                    "status": "Active"
                }
            ]
        })))
        .mount(&server)
        .await;
    // No POST call should be made in dry-run mode.
    Mock::given(method("POST"))
        .and(path("/custom_attributes/blocklist"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: legacy_field\n    type: string\n    deprecated: true\n",
    );

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "custom_attribute"]) // no --confirm
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_custom_attribute_present_in_git_only_is_informational_no_op() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": []
        })))
        .mount(&server)
        .await;
    // PresentInGitOnly is informational drift — Braze has no create
    // endpoint for custom attributes (they materialize on first
    // /users/track), so `apply` must not POST and must not error.
    Mock::given(method("POST"))
        .and(path("/custom_attributes/blocklist"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: typo_attr\n    type: string\n",
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "custom_attribute", "--confirm"])
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
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("in Git registry but not in Braze"),
        "expected PresentInGitOnly warning in plan output; stdout: {stdout}"
    );
}

/// Regression: a registry-only custom_attribute (`PresentInGitOnly`) must
/// not block apply of unrelated, actionable resources in the same run.
/// Before this was reclassified as informational drift, the CA diff
/// short-circuited `check_for_unsupported_ops` and prevented the
/// content_block create POST from firing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_mixed_registry_only_ca_does_not_block_content_block_create() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0,
            "templates": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_block_id": "new-id-1",
            "message": "success"
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/custom_attributes/blocklist"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "fresh", "Hello\n");
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: typo_attr\n    type: string\n",
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--confirm"])
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
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_custom_attribute_metadata_only_is_informational_no_op() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": [
                {
                    "name": "drift",
                    "data_type": "string",
                    "description": "remote desc",
                    "status": "Active"
                }
            ]
        })))
        .mount(&server)
        .await;
    // MetadataOnly is informational drift — `apply` must not POST.
    Mock::given(method("POST"))
        .and(path("/custom_attributes/blocklist"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  - name: drift\n    type: string\n    description: local desc\n",
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "custom_attribute", "--confirm"])
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No actionable changes to apply"),
        "expected informational-drift message; stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_custom_attribute_batches_both_directions() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": [
                {
                    "name": "to_deprecate",
                    "data_type": "string",
                    "status": "Active"
                },
                {
                    "name": "to_reactivate",
                    "data_type": "string",
                    "status": "Blocklisted"
                }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/custom_attributes/blocklist"))
        .and(body_json(json!({
            "custom_attribute_names": ["to_deprecate"],
            "blocklisted": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/custom_attributes/blocklist"))
        .and(body_json(json!({
            "custom_attribute_names": ["to_reactivate"],
            "blocklisted": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(
        tmp.path(),
        "attributes:\n  \
         - name: to_deprecate\n    type: string\n    deprecated: true\n  \
         - name: to_reactivate\n    type: string\n",
    );

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "custom_attribute", "--confirm"])
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
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Applied 2 change(s)"), "stderr: {stderr}");
}

// =====================================================================
// Content Block apply order
// =====================================================================
//
// Topological apply: when a local block references another via the
// Liquid `{{content_blocks.${target}}}` include, the target must be
// created first because Braze validates the body at create time and
// returns an opaque HTTP 500 if the reference doesn't resolve.

async fn received_create_names(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .expect("recording is on by default")
        .iter()
        .filter(|r| {
            r.method == wiremock::http::Method::POST && r.url.path() == "/content_blocks/create"
        })
        .map(|r| {
            let v: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
            v.get("name").and_then(|n| n.as_str()).unwrap().to_string()
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_apply_creates_dependency_target_before_referrer() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_block_id": "new-id",
            "message": "success"
        })))
        .expect(2)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(
        tmp.path(),
        "a_referrer",
        "see {{content_blocks.${b_target} | id: 'cb1'}}\n",
    );
    write_local_content_block(tmp.path(), "b_target", "leaf body\n");

    // Dry-run first: the plan must list b_target before a_referrer so
    // operators see the actual write sequence before passing --confirm.
    let dry_run = tokio::task::spawn_blocking({
        let cfg = config_path.clone();
        move || {
            Command::cargo_bin("braze-sync")
                .unwrap()
                .env("BRAZE_API_KEY", "test-key")
                .args(["--config", cfg.to_str().unwrap()])
                .args(["apply", "--resource", "content_block"])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(dry_run.status.success(), "dry-run must succeed");
    let dry_stdout = String::from_utf8_lossy(&dry_run.stdout);
    let pos_target = dry_stdout
        .find("Content Block: b_target")
        .expect("plan must list b_target");
    let pos_referrer = dry_stdout
        .find("Content Block: a_referrer")
        .expect("plan must list a_referrer");
    assert!(
        pos_target < pos_referrer,
        "dry-run plan must list b_target before a_referrer; got:\n{dry_stdout}"
    );

    tokio::task::spawn_blocking({
        let cfg = config_path.clone();
        move || {
            Command::cargo_bin("braze-sync")
                .unwrap()
                .env("BRAZE_API_KEY", "test-key")
                .args(["--config", cfg.to_str().unwrap()])
                .args(["apply", "--resource", "content_block", "--confirm"])
                .assert()
                .success();
        }
    })
    .await
    .unwrap();

    // Wiremock records requests in receive order, so create-call index
    // is the apply order.
    assert_eq!(
        received_create_names(&server).await,
        vec!["b_target".to_string(), "a_referrer".to_string()],
        "dependency target must be created before referrer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_apply_aborts_on_reference_cycle_before_any_write() {
    // `.expect(0)` on POST proves no create call fires before the
    // cycle is reported.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "cycle_a", "{{content_blocks.${cycle_b}}}\n");
    write_local_content_block(tmp.path(), "cycle_b", "{{content_blocks.${cycle_a}}}\n");

    let output = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .output()
            .unwrap()
    })
    .await
    .unwrap();

    assert!(!output.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("dependency cycle"),
        "expected cycle error in stderr; got:\n{stderr}"
    );
    assert!(
        stderr.contains("cycle_a") && stderr.contains("cycle_b"),
        "cycle error must name both blocks; got:\n{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_apply_alphabetical_opt_out_preserves_input_order() {
    // With `apply_order: alphabetical` the topo pass is skipped; we
    // assert a_referrer comes first (alphabetical), b_target second.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_block_id": "new-id",
            "message": "success"
        })))
        .expect(2)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    // write_config doesn't surface apply_order; hand-write the YAML
    // rather than grow the helper for one test.
    let config_path = tmp.path().join("braze-sync.config.yaml");
    let yaml = format!(
        "version: 1
default_environment: test
environments:
  test:
    api_endpoint: {uri}
    api_key_env: BRAZE_API_KEY
resources:
  content_block:
    enabled: true
    path: content_blocks/
    apply_order: alphabetical
",
        uri = server.uri()
    );
    std::fs::write(&config_path, yaml).unwrap();
    write_local_content_block(
        tmp.path(),
        "a_referrer",
        "see {{content_blocks.${b_target}}}\n",
    );
    write_local_content_block(tmp.path(), "b_target", "leaf\n");

    // Dry-run first: with apply_order=alphabetical the topo pass is
    // skipped so the plan must echo input/alphabetical order.
    let dry_run = tokio::task::spawn_blocking({
        let cfg = config_path.clone();
        move || {
            Command::cargo_bin("braze-sync")
                .unwrap()
                .env("BRAZE_API_KEY", "test-key")
                .args(["--config", cfg.to_str().unwrap()])
                .args(["apply", "--resource", "content_block"])
                .output()
                .unwrap()
        }
    })
    .await
    .unwrap();
    assert!(dry_run.status.success(), "dry-run must succeed");
    let dry_stdout = String::from_utf8_lossy(&dry_run.stdout);
    let pos_referrer = dry_stdout
        .find("Content Block: a_referrer")
        .expect("plan must list a_referrer");
    let pos_target = dry_stdout
        .find("Content Block: b_target")
        .expect("plan must list b_target");
    assert!(
        pos_referrer < pos_target,
        "dry-run plan under alphabetical mode must keep a_referrer before b_target; got:\n{dry_stdout}"
    );

    tokio::task::spawn_blocking({
        let cfg = config_path.clone();
        move || {
            Command::cargo_bin("braze-sync")
                .unwrap()
                .env("BRAZE_API_KEY", "test-key")
                .args(["--config", cfg.to_str().unwrap()])
                .args(["apply", "--resource", "content_block", "--confirm"])
                .assert()
                .success();
        }
    })
    .await
    .unwrap();

    assert_eq!(
        received_create_names(&server).await,
        vec!["a_referrer".to_string(), "b_target".to_string()],
        "alphabetical opt-out must preserve input order"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_apply_resolves_lid_via_new_resource_fallback() {
    // New-resource path: remote is empty so lid resolves via fallback
    // to the URL path tail slug. Apply must POST the *resolved* body
    // (no `__BRAZESYNC__` tokens left).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .and(body_json(json!({
            "name": "promo",
            "content": "<a href=\"https://example.com/cta\">{{x | lid: 'cta'}}</a>\n",
            "tags": [],
            "state": "active"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_block_id": "new-id-1",
            "message": "success"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = common::write_config(tmp.path(), &server.uri());
    common::write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{x | lid: '__BRAZESYNC__'}}</a>\n",
    );

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_apply_falls_back_when_remote_has_fewer_links() {
    // When the local template has more lid placeholders than the
    // remote body has lid values, the extra placeholders resolve to
    // a URL-slug fallback instead of aborting. The diff therefore
    // surfaces a content drift and apply POSTs an update so the
    // remote gains the missing links (Braze reassigns lid values on
    // first dashboard save).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-promo", "name": "promo"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "promo",
            "content": "<p>no anchor here</p>",
            "tags": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/update"))
        .and(body_partial_json(json!({
            "content_block_id": "id-promo",
            "content": "<a href=\"https://example.com/cta\">{{x | lid: 'cta'}}</a>"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = common::write_config(tmp.path(), &server.uri());
    common::write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{x | lid: '__BRAZESYNC__'}}</a>",
    );

    tokio::task::spawn_blocking(move || {
        let assert = Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .assert()
            .success();
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
        // The operator-facing Notice block must summarize the fallback
        // so blind `apply --confirm` runs surface the structural drift.
        assert!(
            stderr.contains("Notice: 1 link(s) resolved with fallback lid"),
            "expected fallback Notice in stderr, got:\n{stderr}"
        );
        assert!(
            stderr.contains("content_block 'promo'"),
            "expected resource scope in Notice, got:\n{stderr}"
        );
        assert!(
            stderr.contains("https://example.com/cta") && stderr.contains("'cta'"),
            "expected URL→fallback mapping in Notice, got:\n{stderr}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_apply_aborts_on_anchor_less_lid_with_remote() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-promo", "name": "promo"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "promo",
            "content": "<p>existing remote body</p>",
            "tags": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = common::write_config(tmp.path(), &server.uri());
    common::write_local_content_block(
        tmp.path(),
        "promo",
        "no link tag here {{x | lid: '__BRAZESYNC__'}} just text",
    );

    tokio::task::spawn_blocking(move || {
        let assert = Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .assert()
            .failure()
            .code(3);
        let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
        assert!(
            stderr.contains("placeholder resolution failure"),
            "expected resolution failure message in stderr, got:\n{stderr}"
        );
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_apply_works_without_values_file_when_no_placeholders() {
    // Backwards compat: existing users without placeholders must keep
    // working with no values file present.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/content_blocks/create"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_block_id": "new-id-1",
            "message": "success"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = common::write_config(tmp.path(), &server.uri());
    common::write_local_content_block(tmp.path(), "plain", "Hello world\n");

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "content_block", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_non_cb_et_kind_ignores_stray_values_directory_files() {
    // Stray files in values/ must not break unrelated commands.
    // Before v0.15, a malformed values/<env>.yaml aborted
    // `apply --resource custom_attribute` even though those kinds
    // never consumed placeholders.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": []
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = common::write_config(tmp.path(), &server.uri());
    write_local_custom_attribute_registry(tmp.path(), "attributes: []\n");
    // Deliberately broken YAML — must NOT be read when running a
    // custom_attribute-only apply.
    common::write_values_file(
        tmp.path(),
        "test",
        ":\n  - this is not: valid: yaml: at all\n",
    );

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", "custom_attribute", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();
}
