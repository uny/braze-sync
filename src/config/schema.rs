//! Raw configuration types deserialized from `braze-sync.config.yaml`.
//!
//! See IMPLEMENTATION.md §10. Every struct here uses
//! `#[serde(deny_unknown_fields)]` — the config file is the **only** place in
//! braze-sync where unknown fields are rejected. Resource files
//! (`schema.yaml`, `template.yaml`, etc.) stay forward-compat permissive
//! per §2.5.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use url::Url;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Schema version. v1.0 binaries accept exactly `1`. Bumping this is a
    /// breaking event by design.
    pub version: u32,
    pub default_environment: String,
    #[serde(default)]
    pub defaults: Defaults,
    pub environments: BTreeMap<String, EnvironmentConfig>,
    #[serde(default)]
    pub resources: ResourcesConfig,
    #[serde(default)]
    pub naming: NamingConfig,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Defaults {}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfig {
    pub api_endpoint: Url,
    /// Name of the environment variable holding the Braze API key. The key
    /// itself MUST NOT live in this file (§2.3 / §10).
    pub api_key_env: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcesConfig {
    #[serde(default = "default_catalog_schema")]
    pub catalog_schema: ResourceConfig,
    #[serde(default = "default_content_block")]
    pub content_block: ResourceConfig,
    #[serde(default = "default_email_template")]
    pub email_template: ResourceConfig,
    #[serde(default = "default_custom_attribute")]
    pub custom_attribute: ResourceConfig,
}

impl ResourcesConfig {
    pub fn is_enabled(&self, kind: crate::resource::ResourceKind) -> bool {
        use crate::resource::ResourceKind;
        match kind {
            ResourceKind::CatalogSchema => self.catalog_schema.enabled,
            ResourceKind::ContentBlock => self.content_block.enabled,
            ResourceKind::EmailTemplate => self.email_template.enabled,
            ResourceKind::CustomAttribute => self.custom_attribute.enabled,
        }
    }
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        Self {
            catalog_schema: default_catalog_schema(),
            content_block: default_content_block(),
            email_template: default_email_template(),
            custom_attribute: default_custom_attribute(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub path: PathBuf,
}

fn default_enabled() -> bool {
    true
}

fn default_catalog_schema() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("catalogs/"),
    }
}

fn default_content_block() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("content_blocks/"),
    }
}

fn default_email_template() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("email_templates/"),
    }
}

fn default_custom_attribute() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("custom_attributes/registry.yaml"),
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamingConfig {
    #[serde(default)]
    pub catalog_name_pattern: Option<String>,
    #[serde(default)]
    pub content_block_name_pattern: Option<String>,
    #[serde(default)]
    pub custom_attribute_name_pattern: Option<String>,
}
