//! Integration tests for `braze-sync validate`.
//!
//! Validate is local-only, so these tests don't need wiremock and run
//! as plain `#[test]` (no async runtime, no spawn_blocking). The
//! interesting bit is that the binary is invoked **without**
//! BRAZE_API_KEY in the environment — `validate` must work in CI
//! contexts (fork PRs) where the secret isn't available.

use assert_cmd::Command;
use std::fs;
use std::path::{Path, PathBuf};

fn write_config(dir: &Path, naming_pattern: Option<&str>) -> PathBuf {
    let config_path = dir.join("braze-sync.config.yaml");
    let mut yaml = String::from(
        "version: 1
default_environment: test
environments:
  test:
    api_endpoint: http://127.0.0.1:1
    api_key_env: BRAZE_VALIDATE_TEST_NOT_SET
",
    );
    if let Some(p) = naming_pattern {
        yaml.push_str(&format!("naming:\n  catalog_name_pattern: \"{p}\"\n"));
    }
    fs::write(&config_path, yaml).unwrap();
    config_path
}

fn write_schema_raw(dir: &Path, dir_name: &str, content: &str) {
    let cat_dir = dir.join("catalogs").join(dir_name);
    fs::create_dir_all(&cat_dir).unwrap();
    fs::write(cat_dir.join("schema.yaml"), content).unwrap();
}

#[test]
fn validate_passes_for_well_formed_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), None);
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
    let config_path = write_config(tmp.path(), None);
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
    let config_path = write_config(tmp.path(), None);
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
    let config_path = write_config(tmp.path(), None);
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
    let config_path = write_config(tmp.path(), Some("^[a-z][a-z0-9_]*$"));
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
