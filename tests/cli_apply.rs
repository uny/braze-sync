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
use common::{write_config, write_local_content_block, write_local_schema};
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
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
async fn content_block_orphan_without_archive_flag_makes_no_write_calls() {
    // Default orphan policy: report-only. The honest-orphan §11.6
    // contract requires zero write calls when --archive-orphans is
    // absent, even with --confirm + --allow-destructive.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-orphan", "name": "legacy"}]
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
    // No local file for "legacy" → orphan from braze-sync's POV.

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
async fn content_block_apply_aborts_on_paginated_list_response() {
    // Pagination is not implemented in v0.2.0. If Braze reports more
    // results than fit on one page, apply MUST abort before any write
    // call: a local file matching a block on page 2+ would otherwise
    // diff as Added and create a duplicate in Braze.
    let server = MockServer::start().await;
    let entries: Vec<serde_json::Value> = (0..100)
        .map(|i| {
            json!({
                "content_block_id": format!("id-{i}"),
                "name": format!("block-{i}")
            })
        })
        .collect();
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 250,
            "content_blocks": entries
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
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
    // A local file whose name doesn't match any of the 100 returned —
    // without the pagination guard, this would diff as Added.
    write_local_content_block(tmp.path(), "block-150", "body\n");

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

    assert!(
        !output.status.success(),
        "expected non-zero exit; stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("pagination"),
        "expected pagination error in stderr, got: {stderr}"
    );
}

/// Custom matcher that asserts an `[ARCHIVED-...]` rename request body
/// has the right shape AND does not carry a `state` field.
///
/// State is local-only per the README and `diff::content_block::syncable_eq`
/// — see `braze::content_block::update_content_block` for the full
/// rationale. The unit test in that module pins state-absence at the
/// `BrazeClient` level; this matcher pins it again at the binary level so
/// a future refactor that bypasses `update_content_block` (or constructs
/// its own request body) can't silently leak state into the wire.
struct ArchiveRenameBody {
    expected_id: &'static str,
    expected_content: &'static str,
    expected_tags: serde_json::Value,
    original_name: &'static str,
}

impl wiremock::Match for ArchiveRenameBody {
    fn matches(&self, request: &wiremock::Request) -> bool {
        let body: serde_json::Value = match serde_json::from_slice(&request.body) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let Some(obj) = body.as_object() else {
            return false;
        };
        if obj.contains_key("state") {
            return false;
        }
        let id_ok = obj.get("content_block_id").and_then(|v| v.as_str()) == Some(self.expected_id);
        let content_ok = obj.get("content").and_then(|v| v.as_str()) == Some(self.expected_content);
        let tags_ok = obj.get("tags") == Some(&self.expected_tags);
        let name_ok = obj
            .get("name")
            .and_then(|v| v.as_str())
            .is_some_and(|n| n.starts_with("[ARCHIVED-") && n.ends_with(self.original_name));
        id_ok && content_ok && tags_ok && name_ok
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_archive_orphans_renames_via_update() {
    // With --archive-orphans, the orphan is renamed via POST /update
    // to `[ARCHIVED-YYYY-MM-DD] <name>`. The body is preserved by
    // first fetching /info — verified by mounting an /info mock.
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
        .and(query_param("content_block_id", "id-orphan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "legacy",
            "content": "preserved body\n",
            "tags": ["pr"]
        })))
        .expect(1)
        .mount(&server)
        .await;
    // Pin the request body shape AND the absence of `state` via
    // ArchiveRenameBody. The name carries today's date so a literal
    // body_json match would couple to the system clock; the matcher
    // checks the dynamic field's prefix/suffix instead.
    Mock::given(method("POST"))
        .and(path("/content_blocks/update"))
        .and(ArchiveRenameBody {
            expected_id: "id-orphan",
            expected_content: "preserved body\n",
            expected_tags: json!(["pr"]),
            original_name: "] legacy",
        })
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());

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
                "--archive-orphans",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_archive_orphans_skips_already_archived_blocks() {
    // Idempotent re-archive: an orphan whose name already carries the
    // [ARCHIVED-YYYY-MM-DD] prefix must NOT trigger another /info or
    // /update call. The unit test for `archive_name` covers the pure
    // function; this test pins the binary-level short-circuit in
    // `apply_content_block` so a future refactor can't regress it
    // (otherwise re-running `apply --archive-orphans` would stamp
    // `[ARCHIVED-today] [ARCHIVED-yesterday] foo`).
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [
                {"content_block_id": "id-old", "name": "[ARCHIVED-2024-01-01] ancient"}
            ]
        })))
        .mount(&server)
        .await;
    // /info must NOT be called — the short-circuit fires before the fetch.
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    // No POST should fire either.
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
            .args([
                "apply",
                "--resource",
                "content_block",
                "--confirm",
                "--archive-orphans",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}
