//! Domain types for the four resources braze-sync manages.
//!
//! See IMPLEMENTATION.md §6 for the complete type contracts. Adding a new
//! variant to [`Resource`] / [`ResourceKind`] forces every `match` site in
//! `diff/`, `fs/`, and `braze/` to be updated — that compiler-enforced
//! exhaustiveness is the central reason braze-sync is written in Rust
//! (§2.4).

pub mod catalog;
pub mod content_block;
pub mod custom_attribute;
pub mod email_template;

pub use catalog::{Catalog, CatalogField, CatalogFieldType};
pub use content_block::{ContentBlock, ContentBlockState};
pub use custom_attribute::{CustomAttribute, CustomAttributeRegistry, CustomAttributeType};
pub use email_template::EmailTemplate;

/// Every resource type braze-sync manages, as a single sum type.
///
/// Adding a variant here will produce match-exhaustiveness errors at every
/// downstream site that consumes a `Resource`. That is intentional.
#[derive(Debug, Clone, PartialEq)]
pub enum Resource {
    CatalogSchema(Catalog),
    ContentBlock(ContentBlock),
    EmailTemplate(EmailTemplate),
    CustomAttributeRegistry(CustomAttributeRegistry),
}

/// Lightweight tag for filtering / CLI args. Mirrors [`Resource`] but
/// without the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
#[clap(rename_all = "snake_case")]
pub enum ResourceKind {
    CatalogSchema,
    ContentBlock,
    EmailTemplate,
    CustomAttribute,
}

impl ResourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CatalogSchema => "catalog_schema",
            Self::ContentBlock => "content_block",
            Self::EmailTemplate => "email_template",
            Self::CustomAttribute => "custom_attribute",
        }
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::CatalogSchema,
            Self::ContentBlock,
            Self::EmailTemplate,
            Self::CustomAttribute,
        ]
    }
}

impl Resource {
    pub fn kind(&self) -> ResourceKind {
        match self {
            Self::CatalogSchema(_) => ResourceKind::CatalogSchema,
            Self::ContentBlock(_) => ResourceKind::ContentBlock,
            Self::EmailTemplate(_) => ResourceKind::EmailTemplate,
            Self::CustomAttributeRegistry(_) => ResourceKind::CustomAttribute,
        }
    }
}
