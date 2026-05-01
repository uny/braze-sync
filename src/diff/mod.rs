//! Pure structural diff layer. No I/O.
//!
//! See IMPLEMENTATION.md §6.6 and §11. The shape of [`DiffOp`] and
//! [`ResourceDiff`] is the central design contract: every diff site in the
//! crate goes through these types so that adding a resource forces all
//! match arms to be updated by the compiler.

use crate::resource::ResourceKind;
use similar::{ChangeTag, TextDiff};

pub mod catalog;
pub mod content_block;
pub mod custom_attribute;
pub mod email_template;
pub mod orphan;

#[derive(Debug, Clone)]
pub struct TextDiffSummary {
    pub additions: usize,
    pub deletions: usize,
}

pub(crate) fn compute_text_diff(from: &str, to: &str) -> TextDiffSummary {
    let diff = TextDiff::from_lines(from, to);
    let mut additions = 0;
    let mut deletions = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    TextDiffSummary {
        additions,
        deletions,
    }
}

/// Treats `None` and `Some("")` as equal — Braze may omit a field or
/// return an empty string interchangeably.
pub(crate) fn opt_str_eq(a: &Option<String>, b: &Option<String>) -> bool {
    a.as_deref().unwrap_or("") == b.as_deref().unwrap_or("")
}

/// Multiset equality: same elements after sort, ignoring order.
pub(crate) fn tags_eq_unordered(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a: Vec<&str> = a.iter().map(String::as_str).collect();
    let mut b: Vec<&str> = b.iter().map(String::as_str).collect();
    a.sort_unstable();
    b.sort_unstable();
    a == b
}

/// A diff operation on a single entity. Polymorphic over the entity type so
/// the same vocabulary applies to whole resources, individual fields, etc.
#[derive(Debug, Clone, PartialEq)]
pub enum DiffOp<T> {
    Added(T),
    Removed(T),
    Modified { from: T, to: T },
    Unchanged,
}

impl<T> DiffOp<T> {
    pub fn is_change(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }

    pub fn is_destructive(&self) -> bool {
        matches!(self, Self::Removed(_))
    }
}

/// Per-resource-kind diff result.
#[derive(Debug, Clone)]
pub enum ResourceDiff {
    CatalogSchema(catalog::CatalogSchemaDiff),
    ContentBlock(content_block::ContentBlockDiff),
    EmailTemplate(email_template::EmailTemplateDiff),
    CustomAttribute(custom_attribute::CustomAttributeDiff),
}

impl ResourceDiff {
    pub fn kind(&self) -> ResourceKind {
        match self {
            Self::CatalogSchema(_) => ResourceKind::CatalogSchema,
            Self::ContentBlock(_) => ResourceKind::ContentBlock,
            Self::EmailTemplate(_) => ResourceKind::EmailTemplate,
            Self::CustomAttribute(_) => ResourceKind::CustomAttribute,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::CatalogSchema(d) => &d.name,
            Self::ContentBlock(d) => &d.name,
            Self::EmailTemplate(d) => &d.name,
            Self::CustomAttribute(d) => &d.name,
        }
    }

    pub fn has_changes(&self) -> bool {
        match self {
            Self::CatalogSchema(d) => d.has_changes(),
            Self::ContentBlock(d) => d.has_changes(),
            Self::EmailTemplate(d) => d.has_changes(),
            Self::CustomAttribute(d) => d.has_changes(),
        }
    }

    /// Whether `apply` can act on this diff. For most resource types this
    /// is the same as `has_changes()`. Custom Attributes are the exception:
    /// only `DeprecationToggled` produces an API call. `MetadataOnly`,
    /// `UnregisteredInGit`, and `PresentInGitOnly` are all informational
    /// drift — Braze has no create endpoint for custom attributes (they
    /// materialize on first `/users/track`), so registry-only entries are
    /// expected and must not block apply.
    pub fn is_actionable(&self) -> bool {
        match self {
            Self::CustomAttribute(d) => d.is_actionable(),
            other => other.has_changes(),
        }
    }

    pub fn has_destructive(&self) -> bool {
        match self {
            Self::CatalogSchema(d) => d.has_destructive(),
            // Content Block / Email Template have no DELETE API. "Destructive"
            // for these resources is reframed as orphan tracking (§11.6); the
            // apply path performs no destructive call.
            Self::ContentBlock(_) => false,
            Self::EmailTemplate(_) => false,
            // Custom Attribute "removal" is only a deprecation flag toggle.
            Self::CustomAttribute(_) => false,
        }
    }

    pub fn is_orphan(&self) -> bool {
        match self {
            Self::ContentBlock(d) => d.is_orphan(),
            Self::EmailTemplate(d) => d.is_orphan(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DiffSummary {
    pub diffs: Vec<ResourceDiff>,
}

impl DiffSummary {
    pub fn changed_count(&self) -> usize {
        self.diffs.iter().filter(|d| d.has_changes()).count()
    }

    /// Count of diffs that `apply` can actually act on. Excludes
    /// informational-only drift (e.g. Custom Attribute metadata-only).
    pub fn actionable_count(&self) -> usize {
        self.diffs.iter().filter(|d| d.is_actionable()).count()
    }

    pub fn destructive_count(&self) -> usize {
        self.diffs.iter().filter(|d| d.has_destructive()).count()
    }

    pub fn orphan_count(&self) -> usize {
        self.diffs.iter().filter(|d| d.is_orphan()).count()
    }

    pub fn in_sync_count(&self) -> usize {
        self.diffs.iter().filter(|d| !d.has_changes()).count()
    }
}
