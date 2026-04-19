//! Configuration loading and environment resolution.
//!
//! Two-step model:
//! 1. [`ConfigFile::load`] reads the YAML, parses it, and runs structural
//!    validation that doesn't need environment variables.
//! 2. [`ConfigFile::resolve`] picks an environment, looks up the API key
//!    via the OS environment, and produces a [`ResolvedConfig`] which is
//!    what the rest of the system consumes.
//!
//! The split exists so tests can drive [`ConfigFile::resolve_with`] with a
//! fake env-lookup closure instead of mutating process-global `std::env`.
//!
//! See IMPLEMENTATION.md §10. The api key is wrapped in
//! [`secrecy::SecretString`] from the moment it leaves the OS so that
//! `Debug`, `tracing`, and panic messages cannot leak it.

pub mod schema;

pub use schema::{
    ConfigFile, Defaults, EnvironmentConfig, NamingConfig, ResourceConfig, ResourcesConfig,
};

use crate::error::{Error, Result};
use secrecy::SecretString;
use std::path::Path;
use url::Url;

/// Fully-resolved config: an environment has been picked and the API key
/// has been pulled out of the OS environment.
#[derive(Debug)]
pub struct ResolvedConfig {
    pub environment_name: String,
    pub api_endpoint: Url,
    /// API key, secrecy-wrapped. Use [`secrecy::ExposeSecret`] at the call
    /// site that needs the plaintext (typically only the BrazeClient
    /// constructor).
    pub api_key: SecretString,
    pub resources: ResourcesConfig,
    pub naming: NamingConfig,
}

impl ConfigFile {
    /// Load and structurally validate a config file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read_to_string(path)?;
        let cfg: ConfigFile =
            serde_norway::from_str(&bytes).map_err(|source| Error::YamlParse {
                path: path.to_path_buf(),
                source,
            })?;
        cfg.validate_static()?;
        Ok(cfg)
    }

    fn validate_static(&self) -> Result<()> {
        if self.version != 1 {
            return Err(Error::Config(format!(
                "unsupported config version {} (this binary supports version 1; \
                 see IMPLEMENTATION.md §2.5 for the forward-compat policy)",
                self.version
            )));
        }
        if !self.environments.contains_key(&self.default_environment) {
            return Err(Error::Config(format!(
                "default_environment '{}' is not declared in the environments map",
                self.default_environment
            )));
        }
        // Validate that all endpoint URLs use http or https. Non-hierarchical
        // schemes (mailto:, data:, etc.) would panic in BrazeClient::url_for
        // when calling path_segments_mut().
        for (name, env) in &self.environments {
            if env.api_key_env.trim().is_empty() {
                return Err(Error::Config(format!(
                    "environment '{name}': api_key_env must not be empty"
                )));
            }
            match env.api_endpoint.scheme() {
                "http" | "https" => {}
                scheme => {
                    return Err(Error::Config(format!(
                        "environment '{name}': api_endpoint must use http or https \
                         (got '{scheme}')"
                    )));
                }
            }
        }
        Ok(())
    }

    /// Resolve to a [`ResolvedConfig`] using the real process environment.
    pub fn resolve(self, env_override: Option<&str>) -> Result<ResolvedConfig> {
        self.resolve_with(env_override, |k| std::env::var(k).ok())
    }

    /// Resolve using a caller-supplied env-var lookup closure. Used by
    /// tests so they don't have to touch process-global `std::env`.
    pub fn resolve_with(
        mut self,
        env_override: Option<&str>,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<ResolvedConfig> {
        let env_name = env_override
            .map(str::to_string)
            .unwrap_or_else(|| self.default_environment.clone());

        if !self.environments.contains_key(&env_name) {
            let known: Vec<&str> = self.environments.keys().map(String::as_str).collect();
            return Err(Error::Config(format!(
                "unknown environment '{}'; declared: [{}]",
                env_name,
                known.join(", ")
            )));
        }
        let env_cfg = self
            .environments
            .remove(&env_name)
            .expect("presence checked immediately above");

        let api_key_str = env_lookup(&env_cfg.api_key_env)
            .ok_or_else(|| Error::MissingEnv(env_cfg.api_key_env.clone()))?;
        if api_key_str.is_empty() {
            return Err(Error::Config(format!(
                "environment variable '{}' is set but empty",
                env_cfg.api_key_env
            )));
        }

        Ok(ResolvedConfig {
            environment_name: env_name,
            api_endpoint: env_cfg.api_endpoint,
            api_key: SecretString::from(api_key_str),
            resources: self.resources,
            naming: self.naming,
        })
    }
}

/// Load `.env` from the current working directory only — no parent
/// traversal — to populate `std::env` before config resolution. A missing
/// file is the common dev case and is not an error.
///
/// IMPLEMENTATION.md §10: via dotenvy, CWD only, no parent traversal.
pub fn load_dotenv() -> Result<()> {
    match dotenvy::from_path(".env") {
        Ok(()) => Ok(()),
        Err(e) if e.not_found() => Ok(()),
        Err(e) => Err(Error::Config(format!(".env load error: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;
    use std::io::Write;

    fn write_config(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    const MINIMAL: &str = r#"
version: 1
default_environment: dev
environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
"#;

    #[test]
    fn loads_minimal_config_with_all_defaults() {
        let f = write_config(MINIMAL);
        let cfg = ConfigFile::load(f.path()).unwrap();
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.default_environment, "dev");
        assert_eq!(cfg.environments.len(), 1);
        // resources defaulted in full
        assert!(cfg.resources.catalog_schema.enabled);
        assert_eq!(
            cfg.resources.catalog_schema.path,
            std::path::PathBuf::from("catalogs/")
        );
        assert_eq!(
            cfg.resources.custom_attribute.path,
            std::path::PathBuf::from("custom_attributes/registry.yaml")
        );
    }

    #[test]
    fn loads_full_config_from_section_10() {
        const FULL: &str = r#"
version: 1
default_environment: dev
environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
  prod:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_PROD_API_KEY
resources:
  catalog_schema:
    enabled: true
    path: catalogs/
  content_block:
    enabled: true
    path: content_blocks/
  email_template:
    enabled: false
    path: email_templates/
  custom_attribute:
    enabled: true
    path: custom_attributes/registry.yaml
naming:
  catalog_name_pattern: "^[a-z][a-z0-9_]*$"
"#;
        let f = write_config(FULL);
        let cfg = ConfigFile::load(f.path()).unwrap();
        assert_eq!(cfg.environments.len(), 2);
        assert!(!cfg.resources.email_template.enabled);
        assert_eq!(
            cfg.naming.catalog_name_pattern.as_deref(),
            Some("^[a-z][a-z0-9_]*$")
        );
    }

    #[test]
    fn rejects_wrong_version() {
        let yaml = r#"
version: 2
default_environment: dev
environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
"#;
        let f = write_config(yaml);
        let err = ConfigFile::load(f.path()).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(err.to_string().contains("version 2"));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let yaml = r#"
version: 1
default_environment: dev
mystery_key: 1
environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
"#;
        let f = write_config(yaml);
        let err = ConfigFile::load(f.path()).unwrap_err();
        assert!(matches!(err, Error::YamlParse { .. }), "got: {err:?}");
    }

    #[test]
    fn rejects_non_http_endpoint_scheme() {
        let yaml = r#"
version: 1
default_environment: dev
environments:
  dev:
    api_endpoint: ftp://rest.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
"#;
        let f = write_config(yaml);
        let err = ConfigFile::load(f.path()).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        let msg = err.to_string();
        assert!(msg.contains("http"), "expected http scheme hint: {msg}");
        assert!(msg.contains("ftp"), "expected actual scheme: {msg}");
    }

    #[test]
    fn rejects_default_environment_not_in_map() {
        let yaml = r#"
version: 1
default_environment: missing
environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
"#;
        let f = write_config(yaml);
        let err = ConfigFile::load(f.path()).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn resolve_uses_default_environment_when_no_override() {
        let f = write_config(MINIMAL);
        let cfg = ConfigFile::load(f.path()).unwrap();
        let resolved = cfg
            .resolve_with(None, |k| {
                assert_eq!(k, "BRAZE_DEV_API_KEY");
                Some("token-abc".into())
            })
            .unwrap();
        assert_eq!(resolved.environment_name, "dev");
        assert_eq!(resolved.api_key.expose_secret(), "token-abc");
    }

    #[test]
    fn resolve_uses_override_when_provided() {
        const TWO_ENVS: &str = r#"
version: 1
default_environment: dev
environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_DEV_API_KEY
  prod:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: BRAZE_PROD_API_KEY
"#;
        let f = write_config(TWO_ENVS);
        let cfg = ConfigFile::load(f.path()).unwrap();
        let resolved = cfg
            .resolve_with(Some("prod"), |k| {
                assert_eq!(k, "BRAZE_PROD_API_KEY");
                Some("prod-token".into())
            })
            .unwrap();
        assert_eq!(resolved.environment_name, "prod");
    }

    #[test]
    fn resolve_unknown_env_lists_known_envs() {
        let f = write_config(MINIMAL);
        let cfg = ConfigFile::load(f.path()).unwrap();
        let err = cfg
            .resolve_with(Some("staging"), |_| Some("x".into()))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("staging"));
        assert!(msg.contains("dev"));
    }

    #[test]
    fn resolve_missing_env_var_is_typed_error() {
        let f = write_config(MINIMAL);
        let cfg = ConfigFile::load(f.path()).unwrap();
        let err = cfg.resolve_with(None, |_| None).unwrap_err();
        match err {
            Error::MissingEnv(name) => assert_eq!(name, "BRAZE_DEV_API_KEY"),
            other => panic!("expected MissingEnv, got {other:?}"),
        }
    }

    #[test]
    fn resolve_empty_env_var_is_rejected() {
        let f = write_config(MINIMAL);
        let cfg = ConfigFile::load(f.path()).unwrap();
        let err = cfg.resolve_with(None, |_| Some(String::new())).unwrap_err();
        assert!(matches!(err, Error::Config(_)));
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn debug_format_does_not_leak_api_key() {
        let f = write_config(MINIMAL);
        let resolved = ConfigFile::load(f.path())
            .unwrap()
            .resolve_with(None, |_| Some("super-secret-token-abc-123".into()))
            .unwrap();
        let dbg = format!("{resolved:?}");
        assert!(
            !dbg.contains("super-secret-token-abc-123"),
            "Debug output leaked api key: {dbg}"
        );
    }

    #[test]
    fn rejects_empty_api_key_env() {
        let yaml = r#"
version: 1
default_environment: dev
environments:
  dev:
    api_endpoint: https://rest.fra-02.braze.eu
    api_key_env: ""
"#;
        let f = write_config(yaml);
        let err = ConfigFile::load(f.path()).unwrap_err();
        assert!(matches!(err, Error::Config(_)), "got: {err:?}");
        assert!(err.to_string().contains("api_key_env"));
    }

    #[test]
    fn load_io_error_for_missing_file() {
        let err = ConfigFile::load("/nonexistent/braze-sync.config.yaml").unwrap_err();
        assert!(matches!(err, Error::Io(_)), "got: {err:?}");
    }
}
