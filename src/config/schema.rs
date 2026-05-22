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
    /// Optional override for the per-env values file location. When unset
    /// the CLI resolves `values/<env>.yaml` relative to the config dir
    /// (per RFC `feat-per-env-values.md` §2.1). The file itself is also
    /// optional — a missing file is OK as long as no resource body
    /// references a `__BRAZESYNC.…__` placeholder.
    #[serde(default)]
    pub values_file: Option<PathBuf>,
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
    #[serde(default = "default_tag")]
    pub tag: ResourceConfig,
}

impl ResourcesConfig {
    pub fn for_kind(&self, kind: crate::resource::ResourceKind) -> &ResourceConfig {
        use crate::resource::ResourceKind;
        match kind {
            ResourceKind::CatalogSchema => &self.catalog_schema,
            ResourceKind::ContentBlock => &self.content_block,
            ResourceKind::EmailTemplate => &self.email_template,
            ResourceKind::CustomAttribute => &self.custom_attribute,
            ResourceKind::Tag => &self.tag,
        }
    }

    pub fn is_enabled(&self, kind: crate::resource::ResourceKind) -> bool {
        self.for_kind(kind).enabled
    }
}

impl Default for ResourcesConfig {
    fn default() -> Self {
        Self {
            catalog_schema: default_catalog_schema(),
            content_block: default_content_block(),
            email_template: default_email_template(),
            custom_attribute: default_custom_attribute(),
            tag: default_tag(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub path: PathBuf,
    /// Regex patterns (matched against resource `name`) that mark a
    /// resource as **managed out of band**. Names matching any pattern
    /// are skipped by `export`, `diff`, `apply`, and `validate` so
    /// Braze reserved attributes (`_unset`) or camelCase duplicates
    /// don't produce noise. See `docs/configuration.md §exclude_patterns`.
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    /// Apply-time ordering policy. Currently consulted only by
    /// `content_block` apply, which uses `Dependency` to topologically
    /// sort `{{content_blocks.${other}}}` references so a referrer is
    /// never created before its target. The field is shared on
    /// `ResourceConfig` (rather than scoped to a content_block-only
    /// type) to keep `ResourcesConfig::for_kind` type-stable; setting
    /// it on other resource kinds is accepted but inert.
    #[serde(default)]
    pub apply_order: ApplyOrder,
}

/// Apply-time ordering policy. `Dependency` topo-sorts content_blocks
/// so a referrer is never created before its target (see
/// `diff::content_block_order`). `Alphabetical` skips that pass and
/// applies in name order — kept for callers who built tooling around
/// the exact apply sequence.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplyOrder {
    #[default]
    Dependency,
    Alphabetical,
}

fn default_enabled() -> bool {
    true
}

fn default_catalog_schema() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("catalogs/"),
        exclude_patterns: Vec::new(),
        apply_order: ApplyOrder::Dependency,
    }
}

fn default_content_block() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("content_blocks/"),
        exclude_patterns: Vec::new(),
        apply_order: ApplyOrder::Dependency,
    }
}

fn default_email_template() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("email_templates/"),
        exclude_patterns: Vec::new(),
        apply_order: ApplyOrder::Dependency,
    }
}

fn default_custom_attribute() -> ResourceConfig {
    ResourceConfig {
        enabled: true,
        path: PathBuf::from("custom_attributes/registry.yaml"),
        exclude_patterns: Vec::new(),
        apply_order: ApplyOrder::Dependency,
    }
}

fn default_tag() -> ResourceConfig {
    // Opt-in: enabling without a registry file would flag every tag
    // reference in existing resources as undeclared on first validate.
    ResourceConfig {
        enabled: false,
        path: PathBuf::from("tags/registry.yaml"),
        exclude_patterns: Vec::new(),
        apply_order: ApplyOrder::Dependency,
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
    #[serde(default)]
    pub tag_name_pattern: Option<String>,
}
