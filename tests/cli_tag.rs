//! Integration tests for the `tag` resource (v0.10.0).
//!
//! Tags are managed in registry mode without any Braze API support
//! (Braze does not expose a public REST endpoint for workspace tags),
//! so these tests run without wiremock and exercise the local
//! aggregation, validate cross-reference, and apply pre-flight paths.

mod common;

use assert_cmd::Command;
use common::{
    write_config_for_validate as write_config, write_local_content_block_with_tags,
    write_local_tag_registry, ValidateNaming,
};

#[test]
fn validate_passes_when_all_referenced_tags_are_in_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), Default::default());

    write_local_content_block_with_tags(tmp.path(), "promo_block", "body", &["campaign", "promo"]);
    write_local_tag_registry(
        tmp.path(),
        "tags:\n  - name: campaign\n  - name: promo\n",
    );

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "tag"])
        .assert()
        .success();
}

#[test]
fn validate_fails_when_resource_references_unregistered_tag() {
    // The whole point of v0.10.0: catch the "tag exists in Git but not in
    // Braze" failure mode at validate time, before apply touches Braze.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), Default::default());

    write_local_content_block_with_tags(
        tmp.path(),
        "dialog_block",
        "body",
        &["campaign", "ad_slot/dialog"],
    );
    write_local_tag_registry(tmp.path(), "tags:\n  - name: campaign\n");

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "tag"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ad_slot/dialog"), "stderr: {stderr}");
    assert!(
        stderr.contains("registry.yaml"),
        "stderr should point at registry: {stderr}"
    );
}

#[test]
fn validate_passes_with_no_registry_file_and_no_tag_references() {
    // Fresh project, no tags used anywhere.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), Default::default());
    write_local_content_block_with_tags(tmp.path(), "plain", "body", &[]);

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "tag"])
        .assert()
        .success();
}

#[test]
fn validate_reports_duplicate_tag_names_in_registry() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(tmp.path(), Default::default());
    write_local_tag_registry(
        tmp.path(),
        "tags:\n  - name: dup\n  - name: dup\n",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "tag"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("duplicate tag name"), "stderr: {stderr}");
}

#[test]
fn validate_reports_naming_pattern_violation() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_config(
        tmp.path(),
        ValidateNaming {
            tag: Some("^[a-z][a-z0-9_/-]*$"),
            ..Default::default()
        },
    );
    write_local_tag_registry(
        tmp.path(),
        "tags:\n  - name: \"BAD UPPER\"\n",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["validate", "--resource", "tag"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("tag_name_pattern"), "stderr: {stderr}");
}
