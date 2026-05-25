//! Integration tests for `braze-sync templatize` (Phase 5).
//!
//! templatize is local-only — no Braze API, no wiremock needed. Tests
//! exercise the file-system effects: in-place body rewrite, canonical
//! values file generation, multi-env skeleton creation, and the
//! idempotency rule that re-runs skip already-templated resources.

mod common;

use assert_cmd::Command;
use common::write_local_content_block;
use std::fs;

/// Write a config that declares two envs so the skeleton path is exercised.
fn write_multi_env_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config_path = dir.join("braze-sync.config.yaml");
    let yaml = r#"version: 1
default_environment: dev
environments:
  dev:
    api_endpoint: http://127.0.0.1:1
    api_key_env: BRAZE_DEV_API_KEY
  prod:
    api_endpoint: http://127.0.0.1:1
    api_key_env: BRAZE_PROD_API_KEY
"#;
    fs::write(&config_path, yaml).unwrap();
    config_path
}

#[test]
fn templatize_rewrites_body_and_writes_canonical_and_skeleton() {
    // v0.15: templatize is rewrite-only. lid / cb_id values are no
    // longer persisted to per-env values files (they are resolved at
    // apply/diff time from the remote body). The body rewrite is the
    // entire effect.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{ x | lid: 'ai8kexrxcp03' }}go</a>",
    );

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize"])
        .assert()
        .success();

    let body = fs::read_to_string(tmp.path().join("content_blocks").join("promo.liquid")).unwrap();
    assert!(
        body.contains("| lid: '__BRAZESYNC__'"),
        "expected placeholder in rewritten body, got:\n{body}"
    );
    assert!(
        !body.contains("ai8kexrxcp03"),
        "raw lid value must be removed from the body, got:\n{body}"
    );

    // No values files are written by templatize in v0.15.
    assert!(!tmp.path().join("values").join("prod.yaml").exists());
    assert!(!tmp.path().join("values").join("dev.yaml").exists());
}

#[test]
fn templatize_dry_run_does_not_touch_files() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{ x | lid: 'ai8kexrxcp03' }}go</a>",
    );
    let before =
        fs::read_to_string(tmp.path().join("content_blocks").join("promo.liquid")).unwrap();

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize", "--dry-run"])
        .assert()
        .success();

    // Body unchanged.
    let after = fs::read_to_string(tmp.path().join("content_blocks").join("promo.liquid")).unwrap();
    assert_eq!(before, after);
    // No values file created.
    assert!(!tmp.path().join("values").join("prod.yaml").exists());
    assert!(!tmp.path().join("values").join("dev.yaml").exists());
}

#[test]
fn templatize_skips_already_templated_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    // Already-templated body: re-running templatize must not double-rewrite.
    write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{ x | lid: '__BRAZESYNC__' }}go</a>",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize"])
        .output()
        .unwrap();
    assert!(output.status.success());

    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("already templated"),
        "expected skip notice on stderr, got:\n{stderr}"
    );

    // Body unchanged.
    let body = fs::read_to_string(tmp.path().join("content_blocks").join("promo.liquid")).unwrap();
    assert!(body.contains("__BRAZESYNC__"));
    // No canonical values file written because no rewrite occurred.
    assert!(!tmp.path().join("values").join("prod.yaml").exists());
}

#[test]
fn templatize_preserves_globals_custom_in_existing_canonical() {
    // v0.15: templatize does not touch any values file. A pre-existing
    // values yaml must round-trip byte-identical.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{ x | lid: 'ai8kexrxcp03' }}go</a>",
    );

    let v_dir = tmp.path().join("values");
    fs::create_dir_all(&v_dir).unwrap();
    let original = "version: 1\ncontent_block:\n  legacy_block:\n    cb_id:\n      shared:\n        value: cb99\n";
    fs::write(v_dir.join("prod.yaml"), original).unwrap();

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize"])
        .assert()
        .success();

    let after = fs::read_to_string(v_dir.join("prod.yaml")).unwrap();
    assert_eq!(after, original, "templatize must not touch values files");
}

#[test]
fn templatize_repeated_cb_id_name_yields_single_key() {
    // Same `${NAME}` referenced twice → both occurrences resolve to the
    // same placeholder key (otherwise apply-time correlation can't match
    // both back to one remote cb_id).
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(
        tmp.path(),
        "duo",
        "header {{content_blocks.${promo} | id: 'cb10'}} \
         footer {{content_blocks.${promo} | id: 'cb10'}}",
    );

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize"])
        .assert()
        .success();

    let body = fs::read_to_string(tmp.path().join("content_blocks").join("duo.liquid")).unwrap();
    let count = body
        .matches("{{content_blocks.${promo} | id: '__BRAZESYNC__'}}")
        .count();
    assert_eq!(
        count, 2,
        "both occurrences must rewrite to the anonymous token, got:\n{body}"
    );
}

#[test]
fn templatize_picks_up_remaining_raw_lid_after_partial_migration() {
    // Mixed state: one placeholder already in place, one raw lid still
    // present. Re-running templatize must finish the migration rather
    // than report "already templated" and skip.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{ x | lid: '__BRAZESYNC__' }}A</a>\n\
         <a href=\"https://example.com/promo\">{{ x | lid: 'rawvalue1234' }}B</a>",
    );

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize"])
        .assert()
        .success();

    let body = fs::read_to_string(tmp.path().join("content_blocks").join("promo.liquid")).unwrap();
    let count = body.matches("| lid: '__BRAZESYNC__'").count();
    assert_eq!(
        count, 2,
        "both lid occurrences must end up as anonymous placeholders, got:\n{body}"
    );
    assert!(
        !body.contains("rawvalue1234"),
        "raw lid value must be removed, got:\n{body}"
    );
}

#[test]
fn templatize_does_not_overwrite_existing_skeleton() {
    // If a non-canonical env already has values populated (e.g. user
    // ran `export --env=dev` after a previous templatize), re-running
    // templatize must NOT clobber it with a fresh `value: null`
    // skeleton.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{ x | lid: 'ai8kexrxcp03' }}go</a>",
    );

    // Pre-populate dev/values.yaml with a real value.
    let v_dir = tmp.path().join("values");
    fs::create_dir_all(&v_dir).unwrap();
    fs::write(
        v_dir.join("dev.yaml"),
        "version: 1\ncontent_block:\n  promo:\n    lid:\n      cta:\n        value: existinglid1\n        url: https://example.com/cta\n",
    )
    .unwrap();

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize"])
        .assert()
        .success();

    let dev = fs::read_to_string(v_dir.join("dev.yaml")).unwrap();
    assert!(
        dev.contains("existinglid1"),
        "existing dev value must be preserved, got:\n{dev}"
    );
}
