//! Integration tests for `braze-sync init`.

use assert_cmd::Command;
use serde_json::json;
use std::fs;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn init_in_empty_dir_creates_full_scaffold() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("braze-sync.config.yaml");

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .arg("init")
        .assert()
        .success();

    assert!(config_path.exists());
    for sub in [
        "catalogs",
        "content_blocks",
        "email_templates",
        "custom_attributes",
    ] {
        assert!(tmp.path().join(sub).is_dir(), "{sub}");
    }
    let gitignore = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert!(gitignore.contains(".env"));
    assert!(gitignore.contains(".env.*"));

    let config_yaml = fs::read_to_string(&config_path).unwrap();
    assert!(config_yaml.contains("version: 1"));
    assert!(config_yaml.contains("api_key_env: BRAZE_DEV_API_KEY"));
}

#[test]
fn init_refuses_to_overwrite_config_without_force() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("braze-sync.config.yaml");
    fs::write(&config_path, "# hand-tuned\nversion: 1\n").unwrap();

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .arg("init")
        .assert()
        .failure();

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("hand-tuned"));
}

#[test]
fn init_force_overwrites_existing_config() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("braze-sync.config.yaml");
    fs::write(&config_path, "# old\n").unwrap();

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["init", "--force"])
        .assert()
        .success();

    let content = fs::read_to_string(&config_path).unwrap();
    assert!(content.contains("braze-sync configuration"));
    assert!(!content.contains("# old"));
}

#[test]
fn init_is_idempotent_for_directories_and_gitignore() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("braze-sync.config.yaml");

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .arg("init")
        .assert()
        .success();

    let gitignore_after_first = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .args(["init", "--force"])
        .assert()
        .success();

    let gitignore_after_second = fs::read_to_string(tmp.path().join(".gitignore")).unwrap();
    assert_eq!(gitignore_after_first, gitignore_after_second);
}

#[test]
fn init_creates_parent_directories_for_nested_config_path() {
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("braze").join("braze-sync.config.yaml");

    Command::cargo_bin("braze-sync")
        .unwrap()
        .args(["--config", config_path.to_str().unwrap()])
        .arg("init")
        .assert()
        .success();

    assert!(config_path.exists());
    assert!(tmp.path().join("braze/catalogs").is_dir());
    assert!(tmp.path().join("braze/.gitignore").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn init_from_existing_pulls_state_into_scaffold() {
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
            "items": []
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
            "templates": []
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/custom_attributes"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "count": 0,
            "custom_attributes": []
        })))
        .mount(&server)
        .await;

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("braze-sync.config.yaml");

    // `init --from-existing` keeps the existing config, so pre-writing one
    // pointing at wiremock survives the scaffold step.
    let yaml = format!(
        "version: 1
default_environment: test
environments:
  test:
    api_endpoint: {}
    api_key_env: BRAZE_DEV_API_KEY
",
        server.uri()
    );
    fs::write(&config_path, yaml).unwrap();

    let tmp_path = tmp.path().to_path_buf();
    let config_path_cmd = config_path.clone();
    tokio::task::spawn_blocking(move || {
        Command::cargo_bin("braze-sync")
            .unwrap()
            .env("BRAZE_DEV_API_KEY", "test-key")
            .args(["--config", config_path_cmd.to_str().unwrap()])
            .args(["init", "--from-existing"])
            .assert()
            .success();
    })
    .await
    .unwrap();

    assert!(tmp_path.join("catalogs").is_dir());
    assert!(tmp_path.join(".gitignore").exists());
    assert!(tmp_path.join("catalogs/cardiology/schema.yaml").exists());
}
