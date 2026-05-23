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
        .args(["templatize", "--from-env", "prod"])
        .assert()
        .success();

    // Body is rewritten in place.
    let body = fs::read_to_string(tmp.path().join("content_blocks").join("promo.liquid")).unwrap();
    assert!(
        body.contains("__BRAZESYNC.lid.cta__"),
        "expected placeholder in rewritten body, got:\n{body}"
    );
    assert!(
        !body.contains("ai8kexrxcp03"),
        "raw lid value must be removed from the body, got:\n{body}"
    );

    // Canonical values file has the actual value.
    let canonical = fs::read_to_string(tmp.path().join("values").join("prod.yaml")).unwrap();
    assert!(
        canonical.contains("ai8kexrxcp03"),
        "canonical (--from-env) values must contain the extracted lid, got:\n{canonical}"
    );
    assert!(
        canonical.contains("https://example.com/cta"),
        "canonical values must carry the URL anchor, got:\n{canonical}"
    );

    // Other env's skeleton has the same key but `value: null`.
    let skeleton = fs::read_to_string(tmp.path().join("values").join("dev.yaml")).unwrap();
    assert!(
        skeleton.contains("cta"),
        "skeleton must mirror the canonical key structure, got:\n{skeleton}"
    );
    assert!(
        skeleton.contains("value: null") || skeleton.contains("value: ~"),
        "skeleton must use `value: null` for non-canonical envs, got:\n{skeleton}"
    );
    assert!(
        !skeleton.contains("ai8kexrxcp03"),
        "skeleton must NOT carry the canonical env's lid value, got:\n{skeleton}"
    );
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
        .args(["templatize", "--from-env", "prod", "--dry-run"])
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
        "<a href=\"https://example.com/cta\">{{ x | lid: '__BRAZESYNC.lid.cta__' }}go</a>",
    );

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize", "--from-env", "prod"])
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
    assert!(body.contains("__BRAZESYNC.lid.cta__"));
    // No canonical values file written because no rewrite occurred.
    assert!(!tmp.path().join("values").join("prod.yaml").exists());
}

#[test]
fn templatize_rejects_unknown_from_env() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(tmp.path(), "promo", "plain body");

    let output = Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize", "--from-env", "staging"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("unknown --from-env"),
        "expected env-not-found error, got:\n{stderr}"
    );
}

#[test]
fn templatize_preserves_globals_custom_in_existing_canonical() {
    // Re-running templatize must NOT clobber operator-curated entries
    // (globals.custom, untouched resource entries) in the canonical
    // values file. It should merge new detections on top instead.
    let tmp = tempfile::tempdir().unwrap();
    let config_path = write_multi_env_config(tmp.path());
    write_local_content_block(
        tmp.path(),
        "promo",
        "<a href=\"https://example.com/cta\">{{ x | lid: 'ai8kexrxcp03' }}go</a>",
    );

    let v_dir = tmp.path().join("values");
    fs::create_dir_all(&v_dir).unwrap();
    fs::write(
        v_dir.join("prod.yaml"),
        "version: 1\n\
         globals:\n  custom:\n    api_host:\n      value: api.example.com\n\
         content_block:\n  legacy_block:\n    cb_id:\n      shared:\n        value: cb99\n",
    )
    .unwrap();

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize", "--from-env", "prod"])
        .assert()
        .success();

    let prod = fs::read_to_string(v_dir.join("prod.yaml")).unwrap();
    assert!(
        prod.contains("api.example.com"),
        "globals.custom must survive merge, got:\n{prod}"
    );
    assert!(
        prod.contains("legacy_block") && prod.contains("cb99"),
        "untouched resource entries must survive merge, got:\n{prod}"
    );
    assert!(
        prod.contains("ai8kexrxcp03"),
        "newly-detected lid must be merged in, got:\n{prod}"
    );
}

#[test]
fn templatize_repeated_cb_id_name_yields_single_key() {
    // Same `${NAME}` referenced twice → one cb_id entry (not name + name_2).
    // Otherwise Phase 3 export refresh leaves the `_2` slot stale forever.
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
        .args(["templatize", "--from-env", "prod"])
        .assert()
        .success();

    let prod = fs::read_to_string(tmp.path().join("values").join("prod.yaml")).unwrap();
    assert!(
        prod.contains("promo:"),
        "expected `promo` cb_id key, got:\n{prod}"
    );
    assert!(
        !prod.contains("promo_2"),
        "repeated ${{promo}} must NOT create a `promo_2` key, got:\n{prod}"
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
        "<a href=\"https://example.com/cta\">{{ x | lid: '__BRAZESYNC.lid.cta__' }}A</a>\n\
         <a href=\"https://example.com/promo\">{{ x | lid: 'rawvalue1234' }}B</a>",
    );

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["templatize", "--from-env", "prod"])
        .assert()
        .success();

    let body = fs::read_to_string(tmp.path().join("content_blocks").join("promo.liquid")).unwrap();
    assert!(
        body.contains("__BRAZESYNC.lid.cta__"),
        "existing placeholder must be preserved, got:\n{body}"
    );
    assert!(
        body.contains("__BRAZESYNC.lid.promo__"),
        "remaining raw lid must now be templated, got:\n{body}"
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
        .args(["templatize", "--from-env", "prod"])
        .assert()
        .success();

    let dev = fs::read_to_string(v_dir.join("dev.yaml")).unwrap();
    assert!(
        dev.contains("existinglid1"),
        "existing dev value must be preserved, got:\n{dev}"
    );
}
