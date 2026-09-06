//! Integration tests for the `diff --plan-out` / `apply --plan` plan-lock.
//!
//! These exercise the §3 contract from
//! `docs/local/feat-apply-plan-locking.md`:
//!
//! - `diff --plan-out=<path>` writes a JSON plan file.
//! - `apply --plan=<path>` succeeds when fresh plan matches saved plan.
//! - mismatched ops, environment, or scope exit 7 and fire no writes.
//!
//! Plus the v2 remote-precondition contract (issue #100): an op whose
//! *shape* is unchanged but whose *remote* moved between plan and apply
//! must exit 7 and fire no writes, while a purely local edit must still
//! be allowed through — the plan binds the remote, not the change set.

mod common;

use assert_cmd::Command;
use common::{
    write_config, write_local_content_block, write_local_email_template, write_local_schema,
};
use predicates::prelude::PredicateBooleanExt;
use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn diff_plan_out_writes_plan_file() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"catalogs": []})))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "newcat", &[("id", "string")]);
    let plan_path = tmp.path().join("plan.json");

    let plan_arg = format!("--plan-out={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", &plan_arg])
            .assert()
            .success();
    })
    .await
    .unwrap();

    let bytes = std::fs::read(&plan_path).expect("plan file written");
    let plan: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(plan["version"], 2);
    assert_eq!(plan["scope"]["environment"], "test");
    // Recorded as `Url` normalizes it — mock server URIs carry no
    // trailing slash, the parsed form does.
    assert_eq!(plan["scope"]["api_endpoint"], format!("{}/", server.uri()));
    assert_eq!(plan["scope"]["resource"], "catalog_schema");
    let ops = plan["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["kind"], "catalog_schema");
    assert_eq!(ops[0]["name"], "newcat");
    assert_eq!(ops[0]["op"], "add");
    // An `add` presupposes absence, not a digest.
    assert_eq!(ops[0]["precondition"]["state"], "absent");
    assert!(ops[0]["precondition"]["digest"].is_null());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_with_matching_plan_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"catalogs": []})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "newcat", &[("id", "string")]);
    let plan_path = tmp.path().join("plan.json");

    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_for_diff = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_for_diff.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", &plan_out])
            .assert()
            .success();
    })
    .await
    .unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
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
                &plan_in,
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_plan_drift_exits_7_and_fires_no_writes() {
    let server = MockServer::start().await;
    // First call (diff): remote is empty → plan has 1 add op.
    // Subsequent calls (apply): remote now has the catalog → fresh plan has 0 ops.
    let initial_state = json!({"catalogs": []});
    let drifted_state = json!({
        "catalogs": [{"name": "newcat", "fields": [{"name": "id", "type": "string"}]}]
    });
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(initial_state))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(drifted_state))
        .mount(&server)
        .await;
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
    write_local_schema(tmp.path(), "newcat", &[("id", "string")]);
    let plan_path = tmp.path().join("plan.json");

    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_for_diff = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_for_diff.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", &plan_out])
            .assert()
            .success();
    })
    .await
    .unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
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
                &plan_in,
            ])
            .assert()
            .failure()
            .code(7);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_plan_environment_mismatch_exits_7_before_api_call() {
    // No mock for /catalogs — if scope-check runs before the API, the test
    // never needs to satisfy a request, which is the property we want.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
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
    let plan_path = tmp.path().join("plan.json");

    // Hand-craft a plan file with a foreign environment.
    let plan_json = serde_json::json!({
        "version": 2,
        "generated_at": "2026-05-18T00:00:00Z",
        "braze_sync_version": env!("CARGO_PKG_VERSION"),
        "scope": {"environment": "prod", "api_endpoint": format!("{}/", server.uri())},
        "ops": []
    });
    std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan_json).unwrap()).unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--confirm", &plan_in])
            .assert()
            .failure()
            .code(7);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_plan_endpoint_mismatch_exits_7_before_api_call() {
    // Issue #106: the environment *name* is a config label, not a
    // workspace identity. Here the name matches on both sides and only
    // the endpoint moved — a name-only scope check would wave this
    // through and apply the plan's observations to a different cluster.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"catalogs": []})))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "newcat", &[("id", "string")]);
    let plan_path = tmp.path().join("plan.json");

    let plan_arg = format!("--plan-out={}", plan_path.display());
    let cfg = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", cfg.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", &plan_arg])
            .assert()
            .success();
    })
    .await
    .unwrap();

    // Repoint environment `test` at a dead endpoint, leaving the name
    // untouched. A dead address is the sharper assertion: if the scope
    // check did not fire first, apply would fail on a connection error,
    // not on exit 7.
    let repointed = write_config(tmp.path(), "http://127.0.0.1:1");
    assert_eq!(repointed, config_path);

    let plan_in = format!("--plan={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--confirm", &plan_in])
            .assert()
            .failure()
            .code(7)
            .stderr(predicates::str::contains("Braze endpoint"));
    })
    .await
    .unwrap();
}

// =====================================================================
// Issue #100: op-shape equality is not evidence about the remote.
// Each test below keeps the op classification identical between plan
// and apply and moves only the remote, so a shape-only lock would let
// the write through.
// =====================================================================

/// Runs `diff --plan-out` then `apply --plan` against the same config,
/// returning the apply assertion for the caller to check.
async fn plan_then_apply(
    config_path: std::path::PathBuf,
    plan_path: std::path::PathBuf,
    resource: &'static str,
) -> assert_cmd::assert::Assert {
    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_for_diff = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_for_diff.to_str().unwrap()])
            .args(["diff", "--resource", resource, &plan_out])
            .assert()
            .success();
    })
    .await
    .unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--resource", resource, "--confirm", &plan_in])
            .assert()
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_block_remote_edit_that_keeps_modify_classification_exits_7() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "content_blocks": [{"content_block_id": "id-promo", "name": "promo"}]
        })))
        .mount(&server)
        .await;
    // Plan time: remote says "old body". Apply time: someone edited it in
    // the web console to "console edit". Local still says "new body", so
    // the op stays `modify` on both runs — the exact #100 precondition.
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .and(query_param("content_block_id", "id-promo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "promo", "content": "old body\n", "tags": []
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/content_blocks/info"))
        .and(query_param("content_block_id", "id-promo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "promo", "content": "console edit\n", "tags": []
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
    write_local_content_block(tmp.path(), "promo", "new body\n");
    let plan_path = tmp.path().join("plan.json");

    plan_then_apply(config_path, plan_path, "content_block")
        .await
        .failure()
        .code(7)
        // Must fail on the precondition, not on op shape: prove the op
        // set itself still matched.
        .stderr(predicates::str::contains("remote state changed"))
        .stderr(predicates::str::contains("remote state moved"))
        .stderr(predicates::str::contains("not in fresh plan").not())
        .stderr(predicates::str::contains("not in saved plan").not());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn email_template_remote_edit_that_keeps_modify_classification_exits_7() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/templates/email/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "templates": [{"email_template_id": "id-welcome", "template_name": "welcome"}]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/templates/email/info"))
        .and(query_param("email_template_id", "id-welcome"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "template_name": "welcome",
            "subject": "Old subject",
            "body": "<p>old</p>",
            "plaintext_body": "old",
            "tags": []
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/templates/email/info"))
        .and(query_param("email_template_id", "id-welcome"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "template_name": "welcome",
            "subject": "Edited in console",
            "body": "<p>old</p>",
            "plaintext_body": "old",
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
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_email_template(tmp.path(), "welcome", "New subject", "<p>new</p>", "new");
    let plan_path = tmp.path().join("plan.json");

    plan_then_apply(config_path, plan_path, "email_template")
        .await
        .failure()
        .code(7)
        .stderr(predicates::str::contains("remote state changed"))
        .stderr(predicates::str::contains("not in fresh plan").not())
        .stderr(predicates::str::contains("not in saved plan").not());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_remote_field_type_change_that_keeps_modify_classification_exits_7() {
    let server = MockServer::start().await;
    // Plan time: remote `price` is a number. Apply time: it is a string.
    // Local wants an extra `sku` field either way, so the op stays
    // `modify` across both runs.
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"catalogs": [{
                "name": "products",
                "fields": [{"name": "id", "type": "string"}, {"name": "price", "type": "number"}]
            }]})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"catalogs": [{
                "name": "products",
                "fields": [{"name": "id", "type": "string"}, {"name": "price", "type": "string"}]
            }]})),
        )
        .mount(&server)
        .await;
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
        "products",
        &[("id", "string"), ("price", "number"), ("sku", "string")],
    );
    let plan_path = tmp.path().join("plan.json");

    plan_then_apply(config_path, plan_path, "catalog_schema")
        .await
        .failure()
        .code(7)
        .stderr(predicates::str::contains("remote state changed"))
        .stderr(predicates::str::contains("not in fresh plan").not())
        .stderr(predicates::str::contains("not in saved plan").not());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_only_edit_between_plan_and_apply_still_applies() {
    // The deliberate boundary of the v2 contract: the plan binds the
    // *remote* preconditions, not the change set. A local edit that
    // keeps the op shape is still applied. Guarantee B — binding the
    // reviewed bytes to the written bytes — is a separate mechanism and
    // is explicitly not claimed here.
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
            "name": "promo", "content": "old body\n", "tags": []
        })))
        .mount(&server)
        .await;
    // Match the body, not just the call: "a write fired" would also pass
    // if apply had pushed the plan-time content, which is precisely the
    // reading this test exists to rule out.
    Mock::given(method("POST"))
        .and(path("/content_blocks/update"))
        .and(body_json(json!({
            "content_block_id": "id-promo",
            "name": "promo",
            "content": "edited after planning\n",
            "tags": []
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
        .expect(1)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_content_block(tmp.path(), "promo", "planned body\n");
    let plan_path = tmp.path().join("plan.json");

    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_for_diff = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_for_diff.to_str().unwrap()])
            .args(["diff", "--resource", "content_block", &plan_out])
            .assert()
            .success();
    })
    .await
    .unwrap();

    // Edit locally after the plan was written; the remote is untouched.
    write_local_content_block(tmp.path(), "promo", "edited after planning\n");

    let plan_in = format!("--plan={}", plan_path.display());
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
                &plan_in,
            ])
            .assert()
            .success();
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v1_plan_file_is_rejected() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    let plan_path = tmp.path().join("plan.json");
    // A v1 plan: correct scope, but its ops carry no evidence about the
    // remote, so this binary cannot check it.
    std::fs::write(
        &plan_path,
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "generated_at": "2026-05-18T00:00:00Z",
            "braze_sync_version": "0.20.0",
            // No `api_endpoint`: a real v1 plan predates the field. The
            // version probe must reject it before the schema parse can
            // complain about a missing field.
            "scope": {"environment": "test"},
            "ops": [{"kind": "content_block", "name": "promo", "op": "modify"}]
        }))
        .unwrap(),
    )
    .unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--confirm", &plan_in])
            .assert()
            .failure()
            // Version skew is a usage error, not plan drift: exit 1,
            // deliberately distinct from the 7 that means "the world
            // moved".
            .code(1)
            .stderr(predicates::str::contains("diff --plan-out"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn v2_plan_missing_a_required_digest_is_rejected_before_any_api_call() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    let plan_path = tmp.path().join("plan.json");
    // A `modify` with no precondition must be rejected as malformed —
    // never silently degraded to the pre-v2 shape-only comparison.
    std::fs::write(
        &plan_path,
        serde_json::to_vec_pretty(&json!({
            "version": 2,
            "generated_at": "2026-05-18T00:00:00Z",
            "braze_sync_version": env!("CARGO_PKG_VERSION"),
            "scope": {"environment": "test", "api_endpoint": format!("{}/", server.uri())},
            "ops": [{"kind": "content_block", "name": "promo", "op": "modify"}]
        }))
        .unwrap(),
    )
    .unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--confirm", &plan_in])
            .assert()
            .failure()
            .code(1)
            .stderr(predicates::str::contains("malformed plan file"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destructive_delete_with_changed_remote_exits_7() {
    // A catalog slated for deletion whose remote schema gained a field
    // between plan and apply. The op stays `destructive_delete`, so only
    // the precondition can catch it.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"catalogs": [{
                "name": "doomed",
                "fields": [{"name": "id", "type": "string"}]
            }]})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"catalogs": [{
            "name": "doomed",
            "fields": [{"name": "id", "type": "string"}, {"name": "added_later", "type": "number"}]
        }]})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    // No local schema at all → the remote catalog is a removal.
    std::fs::create_dir_all(tmp.path().join("catalogs")).unwrap();
    let plan_path = tmp.path().join("plan.json");

    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_for_diff = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_for_diff.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", &plan_out])
            .assert()
            .success();
    })
    .await
    .unwrap();

    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
    assert_eq!(saved["ops"][0]["op"], "destructive_delete");
    assert!(saved["ops"][0]["precondition"]["digest"].is_string());

    let plan_in = format!("--plan={}", plan_path.display());
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
                &plan_in,
            ])
            .assert()
            .failure()
            .code(7)
            .stderr(predicates::str::contains("remote state changed"));
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_field_removal_is_planned_as_destructive_delete() {
    // The op classification must describe what apply will actually do:
    // dropping a field is a destructive write, even though the catalog
    // itself is a `Modified` diff.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"catalogs": [{
                "name": "products",
                "fields": [{"name": "id", "type": "string"}, {"name": "legacy", "type": "number"}]
            }]})),
        )
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(tmp.path(), "products", &[("id", "string")]);
    let plan_path = tmp.path().join("plan.json");

    let plan_out = format!("--plan-out={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", &plan_out])
            .assert()
            .success();
    })
    .await
    .unwrap();

    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
    assert_eq!(saved["ops"].as_array().unwrap().len(), 1);
    assert_eq!(saved["ops"][0]["op"], "destructive_delete");
    assert!(saved["ops"][0]["precondition"]["digest"].is_string());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replaying_a_plan_after_a_partial_catalog_apply_exits_7() {
    // `report_partial_apply` tells the operator that re-running a
    // plan-locked apply exits 7 if anything landed. For a catalog whose
    // fields are written one call at a time that claim was previously
    // false: the surviving ops kept the same shape. The remote
    // precondition is what makes it true.
    let server = MockServer::start().await;
    // Plan + first apply see the bare catalog; after the first field
    // lands, the remote carries it.
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"catalogs": [{
                "name": "products",
                "fields": [{"name": "id", "type": "string"}]
            }]})),
        )
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/catalogs"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"catalogs": [{
                "name": "products",
                "fields": [{"name": "id", "type": "string"}, {"name": "alpha", "type": "string"}]
            }]})),
        )
        .mount(&server)
        .await;
    // First field add succeeds, every later one fails: a partial apply.
    Mock::given(method("POST"))
        .and(path("/catalogs/products/fields"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"message": "success"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/catalogs/products/fields"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    write_local_schema(
        tmp.path(),
        "products",
        &[("id", "string"), ("alpha", "string"), ("beta", "string")],
    );
    let plan_path = tmp.path().join("plan.json");

    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_for_diff = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_for_diff.to_str().unwrap()])
            .args(["diff", "--resource", "catalog_schema", &plan_out])
            .assert()
            .success();
    })
    .await
    .unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
    let config_for_first = config_path.clone();
    let plan_for_first = plan_in.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_for_first.to_str().unwrap()])
            .args([
                "apply",
                "--resource",
                "catalog_schema",
                "--confirm",
                &plan_for_first,
            ])
            .assert()
            .failure();
    })
    .await
    .unwrap();

    // Replay the same plan. `alpha` landed, so the remote no longer
    // matches the precondition even though `products` is still `modify`.
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
                &plan_in,
            ])
            .assert()
            .failure()
            .code(7)
            .stderr(predicates::str::contains("remote state changed"));
    })
    .await
    .unwrap();
}
