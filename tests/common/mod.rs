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
/// Optionally sets the catalog and/or content block naming pattern.
pub fn write_config_for_validate(
    dir: &Path,
    catalog_pattern: Option<&str>,
    content_block_pattern: Option<&str>,
) -> PathBuf {
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
    if catalog_pattern.is_some() || content_block_pattern.is_some() {
        yaml.push_str("naming:\n");
        if let Some(p) = catalog_pattern {
            yaml.push_str(&format!("  catalog_name_pattern: \"{p}\"\n"));
        }
        if let Some(p) = content_block_pattern {
            yaml.push_str(&format!("  content_block_name_pattern: \"{p}\"\n"));
        }
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

/// Write a local content block to `<dir>/content_blocks/<name>.liquid`
/// with a minimal frontmatter and the given body.
pub fn write_local_content_block(dir: &Path, name: &str, body: &str) {
    let cb_dir = dir.join("content_blocks");
    fs::create_dir_all(&cb_dir).unwrap();
    let text = format!("---\nname: {name}\nstate: active\n---\n{body}");
    fs::write(cb_dir.join(format!("{name}.liquid")), text).unwrap();
}

/// Write raw content to `<dir>/content_blocks/<name>.liquid` (no
/// formatting assumptions). For tests that want to construct invalid
/// frontmatter or omit fields.
pub fn write_content_block_raw(dir: &Path, name: &str, content: &str) {
    let cb_dir = dir.join("content_blocks");
    fs::create_dir_all(&cb_dir).unwrap();
    fs::write(cb_dir.join(format!("{name}.liquid")), content).unwrap();
}

/// Write a local catalog items CSV to `<dir>/catalogs/<catalog_name>/items.csv`.
pub fn write_local_items(dir: &Path, catalog_name: &str, csv_content: &str) {
    let cat_dir = dir.join("catalogs").join(catalog_name);
    fs::create_dir_all(&cat_dir).unwrap();
    fs::write(cat_dir.join("items.csv"), csv_content).unwrap();
}

/// Write a local email template directory under `<dir>/email_templates/<name>/`.
pub fn write_local_email_template(
    dir: &Path,
    name: &str,
    subject: &str,
    body_html: &str,
    body_txt: &str,
) {
    let et_dir = dir.join("email_templates").join(name);
    fs::create_dir_all(&et_dir).unwrap();
    let yaml = format!("name: {name}\nsubject: {subject}\n");
    fs::write(et_dir.join("template.yaml"), yaml).unwrap();
    fs::write(et_dir.join("body.html"), body_html).unwrap();
    fs::write(et_dir.join("body.txt"), body_txt).unwrap();
}
