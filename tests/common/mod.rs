#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

/// Write a minimal braze-sync config pointing at the given mock server.
pub fn write_config(dir: &Path, server_uri: &str) -> PathBuf {
    let config_path = dir.join("braze-sync.config.yaml");
    let yaml = format!(
        "version: 1
default_environment: test
environments:
  test:
    api_endpoint: {server_uri}
    api_key_env: BRAZE_API_KEY
"
    );
    fs::write(&config_path, yaml).unwrap();
    config_path
}

/// Write a minimal braze-sync config for validate (no real server needed).
pub fn write_config_for_validate(dir: &Path, naming_pattern: Option<&str>) -> PathBuf {
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

/// Write a local catalog schema.yaml under `<dir>/catalogs/<name>/`.
pub fn write_local_schema(dir: &Path, name: &str, fields: &[(&str, &str)]) {
    let cat_dir = dir.join("catalogs").join(name);
    fs::create_dir_all(&cat_dir).unwrap();
    let mut yaml = format!("name: {name}\nfields:\n");
    for (n, t) in fields {
        yaml.push_str(&format!("  - name: {n}\n    type: {t}\n"));
    }
    fs::write(cat_dir.join("schema.yaml"), yaml).unwrap();
}

/// Write raw content to `<dir>/catalogs/<dir_name>/schema.yaml`.
pub fn write_schema_raw(dir: &Path, dir_name: &str, content: &str) {
    let cat_dir = dir.join("catalogs").join(dir_name);
    fs::create_dir_all(&cat_dir).unwrap();
    fs::write(cat_dir.join("schema.yaml"), content).unwrap();
}
