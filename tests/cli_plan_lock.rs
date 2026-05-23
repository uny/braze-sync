//! Integration tests for the `diff --plan-out` / `apply --plan` plan-lock.
//!
//! These exercise the §3 contract from
//! `docs/local/feat-apply-plan-locking.md`:
//!
//! - `diff --plan-out=<path>` writes a JSON plan file.
//! - `apply --plan=<path>` succeeds when fresh plan matches saved plan.
//! - mismatched ops, environment, or scope exit 7 and fire no writes.

mod common;

use assert_cmd::Command;
use common::{write_config, write_local_schema};
use serde_json::json;
use wiremock::matchers::{method, path};
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
    assert_eq!(plan["version"], 1);
    assert_eq!(plan["scope"]["environment"], "test");
    assert_eq!(plan["scope"]["resource"], "catalog_schema");
    let ops = plan["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["kind"], "catalog_schema");
    assert_eq!(ops[0]["name"], "newcat");
    assert_eq!(ops[0]["op"], "add");
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
        "version": 1,
        "generated_at": "2026-05-18T00:00:00Z",
        "braze_sync_version": env!("CARGO_PKG_VERSION"),
        "scope": {"environment": "prod"},
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
async fn apply_plan_archive_orphans_mismatch_exits_7_before_api_call() {
    // Plan was generated *without* --archive-orphans but apply passes the
    // flag (or vice versa). The frozen op set would imply different
    // writes between the two modes, so the lock must reject before any
    // API call.
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

    let plan_json = serde_json::json!({
        "version": 1,
        "generated_at": "2026-05-18T00:00:00Z",
        "braze_sync_version": env!("CARGO_PKG_VERSION"),
        "scope": {"environment": "test", "archive_orphans": false},
        "ops": []
    });
    std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan_json).unwrap()).unwrap();

    let plan_in = format!("--plan={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", config_path.to_str().unwrap()])
            .args(["apply", "--confirm", "--archive-orphans", &plan_in])
            .assert()
            .failure()
            .code(7);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_lock_aborts_when_consumed_values_change_between_plan_and_apply() {
    // RFC §4 Phase 6: editing values/<env>.yaml after diff --plan-out
    // must abort apply --plan with PlanDrift (exit 7), before any
    // mutation hits Braze.
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
        .expect(0) // No POST may fire on plan drift.
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), &server.uri());
    common::write_local_content_block(
        tmp.path(),
        "promo",
        "cta=__BRAZESYNC.lid.cta__\n",
    );
    common::write_values_file(
        tmp.path(),
        "test",
        r#"version: 1
content_block:
  promo:
    lid:
      cta:
        value: oldlidvalue1
        url: https://example.com/cta
"#,
    );

    let plan_path = tmp.path().join("plan.json");
    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_str = config_path.to_str().unwrap().to_string();
    let plan_out_clone = plan_out.clone();
    let config_str_clone = config_str.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", &config_str_clone])
            .args(["diff", "--resource", "content_block", &plan_out_clone])
            .assert()
            .success();
    })
    .await
    .unwrap();

    // Edit the values file after the plan has been written.
    common::write_values_file(
        tmp.path(),
        "test",
        r#"version: 1
content_block:
  promo:
    lid:
      cta:
        value: tamperedlid
        url: https://example.com/cta
"#,
    );

    let plan_in = format!("--plan={}", plan_path.display());
    let assert = tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", &config_str])
            .args(["apply", "--resource", "content_block", "--confirm", &plan_in])
            .assert()
            .failure()
            .code(7)
    })
    .await
    .unwrap();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        stderr.contains("plan drift")
            && (stderr.contains("values inputs changed")
                || stderr.contains("consumed values")),
        "expected plan-drift values message, got:\n{stderr}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plan_lock_passes_when_unrelated_values_key_changes() {
    // Editing a values key that no resource currently references
    // (orphan in one resource's plan world) must NOT trigger
    // plan-drift abort. The hash is per-resource over its CONSUMED
    // subset.
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
    let config_path = write_config(tmp.path(), &server.uri());
    common::write_local_content_block(
        tmp.path(),
        "promo",
        "cta=__BRAZESYNC.lid.cta__\n",
    );
    // Two entries; only `cta` is consumed by the placeholder.
    common::write_values_file(
        tmp.path(),
        "test",
        r#"version: 1
content_block:
  promo:
    lid:
      cta:
        value: stableidvalue
        url: https://example.com/cta
      unused:
        value: othervaluexxx
        url: https://example.com/unused
"#,
    );

    let plan_path = tmp.path().join("plan.json");
    let plan_out = format!("--plan-out={}", plan_path.display());
    let config_str = config_path.to_str().unwrap().to_string();
    let plan_out_clone = plan_out.clone();
    let config_str_clone = config_str.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", &config_str_clone])
            .args(["diff", "--resource", "content_block", &plan_out_clone])
            .assert()
            .success();
    })
    .await
    .unwrap();

    // Edit only the *unused* entry. The plan-lock hash for `promo`
    // must not change because `unused` is not in its consumed set.
    common::write_values_file(
        tmp.path(),
        "test",
        r#"version: 1
content_block:
  promo:
    lid:
      cta:
        value: stableidvalue
        url: https://example.com/cta
      unused:
        value: editedvaluexx
        url: https://example.com/unused
"#,
    );

    let plan_in = format!("--plan={}", plan_path.display());
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_API_KEY", "test-key")
            .args(["--config", &config_str])
            .args(["apply", "--resource", "content_block", "--confirm", &plan_in])
            .assert()
            .success();
    })
    .await
    .unwrap();
}
