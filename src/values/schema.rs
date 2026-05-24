//! YAML schema types for `values/<env>.yaml`.
//!
//! v0.15 model: only user-managed namespaces (`custom`, `global`) live in
//! values yaml. lid / cb_id are Braze-owned and resolved at apply/diff
//! time from the remote body — they never round-trip through Git.
//!
//! Forward-compat policy: `#[serde(deny_unknown_fields)]` is intentionally
//! NOT applied here. Pre-v0.15 values files carried `lid:` / `cb_id:`
//! sections under each resource and under email_template field scopes;
//! we silently drop those at parse time so old files keep loading until
//! the operator runs `braze-sync values cleanup` to physically remove
//! them. Unknown keys at the resource / globals level fall through the
//! same default-ignore behavior.

use regex_lite::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::error::{Error, Result};

/// Currently supported values file schema version.
pub const SUPPORTED_VERSION: u32 = 1;

/// Top-level `values/<env>.yaml` document.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ValuesFile {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Globals::is_empty")]
    pub globals: Globals,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub content_block: BTreeMap<String, ContentBlockValues>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub email_template: BTreeMap<String, EmailTemplateValues>,
}

/// Cross-resource per-env values. Currently only `custom` is populated.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Globals {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, CustomEntry>,
}

impl Globals {
    fn is_empty(&self) -> bool {
        self.custom.is_empty()
    }
}

/// Resource-scoped values for a content_block. Only user-managed
/// `custom` survives in values yaml; lid/cb_id are resolved from the
/// remote body at apply/diff time.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ContentBlockValues {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, CustomEntry>,
}

/// Resource-scoped values for an email_template. Same shape as
/// content_block — only `custom` lives here.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct EmailTemplateValues {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom: BTreeMap<String, CustomEntry>,
}

/// User-managed custom value; opaque string.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CustomEntry {
    pub value: Option<String>,
}

fn key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("key regex is valid"))
}

impl ValuesFile {
    /// Load and validate a values file from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let parsed: ValuesFile =
            serde_norway::from_str(&raw).map_err(|source| Error::YamlParse {
                path: path.to_path_buf(),
                source,
            })?;
        parsed.validate(path)?;
        Ok(parsed)
    }

    /// Serialize and atomically write to `path` (tmp + `rename(2)`).
    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate(path)?;
        let yaml = serde_norway::to_string(self).map_err(|e| Error::InvalidFormat {
            path: path.to_path_buf(),
            message: format!("serializing values file: {e}"),
        })?;
        crate::fs::write_atomic(path, yaml.as_bytes())
    }

    /// Validate version + key shapes.
    pub fn validate(&self, path: &Path) -> Result<()> {
        if self.version != SUPPORTED_VERSION {
            return Err(Error::InvalidFormat {
                path: path.to_path_buf(),
                message: format!(
                    "values file requires schema version {} (found: {})",
                    SUPPORTED_VERSION, self.version
                ),
            });
        }

        let mut errors: Vec<String> = Vec::new();

        for key in self.globals.custom.keys() {
            check_key(key, "globals.custom", &mut errors);
        }
        for (cb_name, cb) in &self.content_block {
            let scope = format!("content_block.{cb_name}.custom");
            for key in cb.custom.keys() {
                check_key(key, &scope, &mut errors);
            }
        }
        for (et_name, et) in &self.email_template {
            let scope = format!("email_template.{et_name}.custom");
            for key in et.custom.keys() {
                check_key(key, &scope, &mut errors);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error::InvalidFormat {
                path: path.to_path_buf(),
                message: errors.join("; "),
            })
        }
    }
}

fn check_key(key: &str, scope: &str, errors: &mut Vec<String>) {
    if !key_re().is_match(key) {
        errors.push(format!("{scope}: key '{key}' must match [a-z][a-z0-9_]*"));
    }
}

/// Default location resolver: `values_file` config field wins,
/// otherwise `values/<env>.yaml` relative to the config dir.
pub fn default_values_path(config_dir: &Path, env: &str) -> PathBuf {
    config_dir.join("values").join(format!("{env}.yaml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_temp(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parses_minimal_valid_file() {
        let f = write_temp("version: 1\n");
        let parsed = ValuesFile::load(f.path()).unwrap();
        assert_eq!(parsed.version, 1);
        assert!(parsed.content_block.is_empty());
        assert!(parsed.email_template.is_empty());
    }

    #[test]
    fn parses_custom_and_globals() {
        let f = write_temp(
            r#"
version: 1
globals:
  custom:
    api_host:
      value: api-prod.example.com
content_block:
  cb_promo_banner:
    custom:
      banner_variant:
        value: A
email_template:
  welcome:
    custom:
      user_segment_id:
        value: seg_prod_42
"#,
        );
        let parsed = ValuesFile::load(f.path()).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(
            parsed.globals.custom["api_host"].value.as_deref(),
            Some("api-prod.example.com")
        );
        assert_eq!(
            parsed.content_block["cb_promo_banner"].custom["banner_variant"]
                .value
                .as_deref(),
            Some("A")
        );
        assert_eq!(
            parsed.email_template["welcome"].custom["user_segment_id"]
                .value
                .as_deref(),
            Some("seg_prod_42")
        );
    }

    #[test]
    fn silently_drops_legacy_lid_cb_id_sections() {
        // Pre-v0.15 values files: lid/cb_id sections must parse (and
        // be dropped) so operators can migrate without a hard break.
        // `braze-sync values cleanup` is what physically removes them.
        let f = write_temp(
            r#"
version: 1
content_block:
  cb_promo_banner:
    lid:
      spring_sale:
        value: ai8kexrxcp03
        url: https://example.com/spring-sale
    cb_id:
      cb_promo_image:
        value: cb42
    custom:
      banner_variant:
        value: A
email_template:
  welcome:
    subject:
      lid:
        promo_subject:
          value: lidsubj42
    body_html:
      cb_id:
        cb_promo_image:
          value: cb42
    custom:
      seg:
        value: s
"#,
        );
        let parsed = ValuesFile::load(f.path()).unwrap();
        assert_eq!(
            parsed.content_block["cb_promo_banner"].custom["banner_variant"]
                .value
                .as_deref(),
            Some("A")
        );
        assert_eq!(
            parsed.email_template["welcome"].custom["seg"]
                .value
                .as_deref(),
            Some("s")
        );
    }

    #[test]
    fn rejects_unsupported_version() {
        let f = write_temp("version: 2\n");
        let err = ValuesFile::load(f.path()).unwrap_err();
        match err {
            Error::InvalidFormat { message, .. } => {
                assert!(message.contains("schema version"));
            }
            other => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    #[test]
    fn rejects_bad_key_shape() {
        let f = write_temp(
            r#"
version: 1
content_block:
  cb:
    custom:
      BadKey:
        value: x
"#,
        );
        let err = ValuesFile::load(f.path()).unwrap_err();
        match err {
            Error::InvalidFormat { message, .. } => {
                assert!(message.contains("BadKey"));
            }
            other => panic!("expected InvalidFormat, got {other:?}"),
        }
    }

    #[test]
    fn yaml_parse_error_surfaces() {
        let f = write_temp(":\n  unbalanced");
        let err = ValuesFile::load(f.path()).unwrap_err();
        assert!(matches!(err, Error::YamlParse { .. }));
    }

    #[test]
    fn save_omits_empty_namespaces() {
        let mut vf = ValuesFile {
            version: 1,
            ..Default::default()
        };
        let mut cb = ContentBlockValues::default();
        cb.custom.insert(
            "variant".to_string(),
            CustomEntry {
                value: Some("A".into()),
            },
        );
        vf.content_block.insert("promo".into(), cb);

        let s = serde_norway::to_string(&vf).unwrap();
        assert!(!s.contains("globals"), "empty globals leaked: {s}");
        assert!(
            !s.contains("email_template"),
            "empty email_template leaked: {s}"
        );
        assert!(s.contains("value: A"));
    }

    #[test]
    fn default_path_uses_env_name() {
        let p = default_values_path(Path::new("/tmp/repo"), "prod");
        assert_eq!(p, PathBuf::from("/tmp/repo/values/prod.yaml"));
    }
}
