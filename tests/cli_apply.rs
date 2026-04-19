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
    write_local_email_template, write_local_items, write_local_schema,
};
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
    // absent, even with --confirm + --allow-destructive — the only
    // knob that turns archival on is --archive-orphans itself.
    // (Content Block has no destructive ops, so --allow-destructive
    // is operationally a no-op for this resource; passing it here
    // pins that an operator who confuses the two flags can't
    // accidentally trigger an archive rename.)
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
        // Parse the archive prefix strictly: `[ARCHIVED-YYYY-MM-DD] <original>`.
        // A looser "starts_with / ends_with" match would happily accept
        // `[ARCHIVED-foo] legacy` or `[ARCHIVED-] legacy`, which would
        // silently tolerate a bug in the date formatter.
        let name_ok = obj.get("name").and_then(|v| v.as_str()).is_some_and(|n| {
            let Some(rest) = n.strip_prefix("[ARCHIVED-") else {
                return false;
            };
            let Some((date, tail)) = rest.split_once("] ") else {
                return false;
            };
            tail == self.original_name && looks_like_iso_date(date)
        });
        id_ok && content_ok && tags_ok && name_ok
    }
}

/// Permissive ISO-shape check for `YYYY-MM-DD`. Doesn't validate the
/// actual calendar date — an off-by-one month is already caught by the
/// `diff::orphan` unit tests; this matcher's job is to pin the wire
/// shape, not to re-test chrono.
fn looks_like_iso_date(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..].iter().all(|b| b.is_ascii_digit())
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
            original_name: "legacy",
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
async fn email_template_archive_orphans_renames_via_update() {
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
        .and(query_param("email_template_id", "id-old"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "template_name": "old_promo",
            "subject": "Old",
            "body": "<p>Old</p>",
            "plaintext_body": "Old",
            "tags": [],
            "message": "success"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/templates/email/update"))
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
                "email_template",
                "--confirm",
                "--archive-orphans",
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

// =================================================================
// Catalog Items apply tests
// =================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_catalog_items_upserts_added_items() {
    let server = MockServer::start().await;
    // list_catalogs for discovery
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "catalogs": [
                {"name": "cardiology", "fields": [{"name": "id", "type": "string"}]}
            ]
        })))
        .mount(&server)
        .await;
    // Remote has no items
    Mock::given(method("GET"))
        .and(path("/catalogs/cardiology/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "items": [],
            "message": "success"
        })))
        .mount(&server)
        .await;
    // Expect an upsert POST
    let upsert_mock = Mock::given(method("POST"))
        .and(path("/catalogs/cardiology/items"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount_as_scoped(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_items(tmp.path(), "cardiology", "id,name\naf001,atrial\n");
    let config_str = config_path.to_str().unwrap().to_string();

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", &config_str])
            .args(["apply", "--resource", "catalog_items", "--confirm"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    // The scoped mock asserts expect(1) on drop.
    drop(upsert_mock);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_catalog_items_dry_run_does_not_upsert() {
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
    // Expect zero upsert POSTs (dry-run)
    let upsert_mock = Mock::given(method("POST"))
        .and(path("/catalogs/cardiology/items"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount_as_scoped(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_items(tmp.path(), "cardiology", "id,name\naf001,atrial\n");
    let config_str = config_path.to_str().unwrap().to_string();

    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", &config_str])
            .args(["apply", "--resource", "catalog_items"]) // no --confirm
            .assert()
            .success();
    })
    .await
    .unwrap();

    drop(upsert_mock);
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
            "count": 1,
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
            "count": 1,
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
async fn apply_custom_attribute_present_in_git_only_rejects() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "attributes": []
        })))
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

    assert_eq!(
        output.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be created via API"),
        "stderr: {stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_custom_attribute_metadata_only_is_informational_no_op() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 1,
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
            "count": 2,
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
