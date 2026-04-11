//! Integration tests for `braze-sync validate`.
//!
//! Validate is local-only, so these tests don't need wiremock and run
//! as plain `#[test]` (no async runtime, no spawn_blocking). The
//! interesting bit is that the binary is invoked **without**
//! BRAZE_API_KEY in the environment — `validate` must work in CI
//! contexts (fork PRs) where the secret isn't available.

mod common;

use assert_cmd::Command;
use common::{
    write_config_for_validate as write_config, write_content_block_raw, write_local_content_block,
    write_schema_raw,
};

#[test]
fn validate_passes_for_well_formed_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    write_schema_raw(
        tmp.path(),
        "cardiology",
        "name: cardiology\nfields:\n  - name: id\n    type: string\n",
    );

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "catalog_schema"])
        .assert()
        .success();
}

#[test]
fn validate_does_not_require_braze_api_key() {
    // Same as the happy path, but explicit: no env var is set, and
    // validate still succeeds because cli::run skips env resolution
    // for the Validate subcommand.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    write_schema_raw(
        tmp.path(),
        "x",
        "name: x\nfields:\n  - name: id\n    type: string\n",
    );

    Command::cargo_bin("braze-sync")
        .unwrap()
        .env_remove("BRAZE_VALIDATE_TEST_NOT_SET")
        .env_remove("BRAZE_API_KEY")
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate"])
        .assert()
        .success();
}

#[test]
fn validate_reports_yaml_parse_error_and_exits_3() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    // `fields:` declared as a scalar instead of a list — fails serde
    // deserialization at the field-vector level.
    write_schema_raw(tmp.path(), "broken", "name: broken\nfields: not_a_list\n");

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "catalog_schema"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("parse error"), "stderr: {stderr}");
    assert!(stderr.contains("broken"), "stderr: {stderr}");
}

#[test]
fn validate_reports_name_directory_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    write_schema_raw(
        tmp.path(),
        "directory_name",
        "name: yaml_name\nfields:\n  - name: id\n    type: string\n",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "catalog_schema"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("yaml_name"), "stderr: {stderr}");
    assert!(stderr.contains("directory_name"), "stderr: {stderr}");
}

#[test]
fn validate_reports_naming_pattern_violation() {
    let tmp = tempfile::tempdir().unwrap();
    // The pattern allows lowercase + digits + underscore only.
    let config_path = write_config(tmp.path(), Some("^[a-z][a-z0-9_]*$"), None);
    write_schema_raw(
        tmp.path(),
        "BadName",
        "name: BadName\nfields:\n  - name: id\n    type: string\n",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "catalog_schema"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("BadName"), "stderr: {stderr}");
    assert!(stderr.contains("catalog_name_pattern"), "stderr: {stderr}");
}

// =====================================================================
// Content Block (v0.2.0)
// =====================================================================

#[test]
fn validate_passes_for_well_formed_content_block() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    write_local_content_block(tmp.path(), "promo", "Hello\n");

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "content_block"])
        .assert()
        .success();
}

#[test]
fn validate_content_block_does_not_require_braze_api_key() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    write_local_content_block(tmp.path(), "promo", "Hello\n");

    Command::cargo_bin("braze-sync")
        .unwrap()
        .env_remove("BRAZE_VALIDATE_TEST_NOT_SET")
        .env_remove("BRAZE_API_KEY")
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "content_block"])
        .assert()
        .success();
}

#[test]
fn validate_reports_frontmatter_parse_error() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    // Missing closing fence
    write_content_block_raw(
        tmp.path(),
        "broken",
        "---\nname: broken\nstate: active\nbody never closes\n",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "content_block"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("parse error"), "stderr: {stderr}");
    assert!(stderr.contains("broken"), "stderr: {stderr}");
}

#[test]
fn validate_reports_content_block_name_file_stem_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, None);
    // file is on_disk_name.liquid, frontmatter says yaml_name
    write_content_block_raw(
        tmp.path(),
        "on_disk_name",
        "---\nname: yaml_name\nstate: active\n---\nbody\n",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "content_block"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("yaml_name"), "stderr: {stderr}");
    assert!(stderr.contains("on_disk_name"), "stderr: {stderr}");
}

#[test]
fn validate_reports_content_block_naming_pattern_violation() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None, Some("^[a-z][a-z0-9_]*$"));
    write_local_content_block(tmp.path(), "BadName", "x");

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "content_block"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("BadName"), "stderr: {stderr}");
    assert!(
        stderr.contains("content_block_name_pattern"),
        "stderr: {stderr}"
    );
}
