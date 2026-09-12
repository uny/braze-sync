//! Pure structural diff layer. No I/O.
//!
//! The shape of [`DiffOp`] and [`ResourceDiff`] is the central design
//! contract: every diff site in the crate goes through these types so that
//! adding a resource forces all match arms to be updated by the compiler.

use crate::resource::ResourceKind;
use similar::{ChangeTag, TextDiff};

pub mod catalog;
pub mod content_block;
pub mod content_block_order;
pub mod custom_attribute;
pub mod digest;
pub mod email_template;
pub mod plan;
pub mod tag;

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

/// How a changed diff is treated by `diff --fail-on-drift`.
///
/// Separate from [`ResourceDiff::is_actionable`], which answers a
/// different question — "can `apply` push this?" — and excludes orphans
/// and all Tag drift, both of which a CI gate must keep failing on.
///
/// The rule for [`DriftTier::ReportOnly`] is deliberately narrow: a
/// state qualifies only when a *correct* configuration produces it —
/// when the same output is what a healthy setup looks like, failing
/// the build over it carries no information. "braze-sync cannot write
/// it" is not sufficient — most unwritable drift still means a human
/// must go do something, in the Braze dashboard or in Git.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftTier {
    /// No change. Not drift at all.
    None,
    /// Real disagreement, but one a correct setup produces — so it
    /// cannot be told apart from a mistake by looking. Listed in every
    /// output; does not raise exit 2.
    ReportOnly,
    /// Drift a human must act on. Raises exit 2 under `--fail-on-drift`.
    Gating,
}

/// Per-resource-kind diff result.
#[derive(Debug, Clone)]
pub enum ResourceDiff {
    CatalogSchema(catalog::CatalogSchemaDiff),
    ContentBlock(content_block::ContentBlockDiff),
    EmailTemplate(email_template::EmailTemplateDiff),
    CustomAttribute(custom_attribute::CustomAttributeDiff),
    Tag(tag::TagDiff),
}

impl ResourceDiff {
    pub fn kind(&self) -> ResourceKind {
        match self {
            Self::CatalogSchema(_) => ResourceKind::CatalogSchema,
            Self::ContentBlock(_) => ResourceKind::ContentBlock,
            Self::EmailTemplate(_) => ResourceKind::EmailTemplate,
            Self::CustomAttribute(_) => ResourceKind::CustomAttribute,
            Self::Tag(_) => ResourceKind::Tag,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::CatalogSchema(d) => &d.name,
            Self::ContentBlock(d) => &d.name,
            Self::EmailTemplate(d) => &d.name,
            Self::CustomAttribute(d) => &d.name,
            Self::Tag(d) => &d.name,
        }
    }

    pub fn has_changes(&self) -> bool {
        match self {
            Self::CatalogSchema(d) => d.has_changes(),
            Self::ContentBlock(d) => d.has_changes(),
            Self::EmailTemplate(d) => d.has_changes(),
            Self::CustomAttribute(d) => d.has_changes(),
            Self::Tag(d) => d.has_changes(),
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
        // Orphans are drift (they count as changed), but `apply` can never
        // act on them regardless of kind: Braze exposes no DELETE and
        // renaming is unsafe. Routing through `is_orphan()` keeps the
        // orphan-capable variant set in one place. Excluding them here makes
        // an orphan-only run report "no actionable changes" rather than a
        // misleading DRY RUN / "pass --confirm" that would apply nothing.
        if self.is_orphan() {
            return false;
        }
        match self {
            Self::CustomAttribute(d) => d.is_actionable(),
            // Tag drift is informational from apply's perspective: Braze has
            // no tag-mutation API, so `apply --kind tag` cannot push changes.
            // Pre-flight (in cli/apply.rs) consumes Tag diffs separately to
            // block apply when a referenced tag is unregistered.
            Self::Tag(_) => false,
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
            // Tags have no API mutation at all.
            Self::Tag(_) => false,
        }
    }

    pub fn is_orphan(&self) -> bool {
        match self {
            Self::ContentBlock(d) => d.is_orphan(),
            Self::EmailTemplate(d) => d.is_orphan(),
            _ => false,
        }
    }

    /// Which drift tier this diff falls in. See [`DriftTier`] for the
    /// rule; the per-kind reasoning is below.
    pub fn drift_tier(&self) -> DriftTier {
        if !self.has_changes() {
            return DriftTier::None;
        }
        match self {
            // The only kind with a report-only state; see
            // `CustomAttributeDiff::drift_tier`.
            Self::CustomAttribute(d) => d.drift_tier(),
            // Orphans: Braze exposes no DELETE, but the resolution is a
            // human archiving the resource in the dashboard (or
            // deleting the local file). Somebody must act, so this
            // gates.
            //
            // Tags: `ReferencedButUnregistered` blocks `apply`
            // pre-flight outright, and `RegisteredButUnreferenced` is a
            // registry entry whose last user is gone. Both are resolved
            // entirely in Git plus the dashboard — nothing about them
            // is unresolvable, so both gate.
            //
            // Catalog Schema / Content Block / Email Template: `apply`
            // writes these directly.
            _ => DriftTier::Gating,
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

    /// Count of diffs that raise exit 2 under `--fail-on-drift`. A
    /// subset of [`Self::changed_count`]: the difference is drift a
    /// correct setup produces, which stays in every listing but must
    /// not keep a scheduled CI job permanently red.
    pub fn gating_drift_count(&self) -> usize {
        self.diffs
            .iter()
            .filter(|d| d.drift_tier() == DriftTier::Gating)
            .count()
    }

    /// Count of changed diffs that are listed but do not raise exit 2.
    pub fn report_only_drift_count(&self) -> usize {
        self.diffs
            .iter()
            .filter(|d| d.drift_tier() == DriftTier::ReportOnly)
            .count()
    }
}

#[cfg(test)]
mod drift_tier_tests {
    use super::*;

    fn orphan_content_block() -> ResourceDiff {
        ResourceDiff::ContentBlock(content_block::ContentBlockDiff {
            name: "legacy_banner".into(),
            op: DiffOp::Unchanged,
            text_diff: None,
            orphan: true,
        })
    }

    fn tag(op: tag::TagOp) -> ResourceDiff {
        ResourceDiff::Tag(tag::TagDiff {
            name: "promo".into(),
            op,
            hints: vec![],
        })
    }

    /// An orphan cannot be written by `apply` — Braze exposes no DELETE
    /// — but archiving it in the dashboard resolves it, so it must keep
    /// raising exit 2. This is the case that makes `is_actionable()`
    /// (which excludes orphans) the wrong predicate for the gate.
    #[test]
    fn orphans_gate_even_though_apply_cannot_write_them() {
        let d = orphan_content_block();
        assert!(!d.is_actionable());
        assert_eq!(d.drift_tier(), DriftTier::Gating);
    }

    /// Braze has no tag-mutation API at all, yet both Tag states are
    /// resolved entirely in Git plus the dashboard. `is_actionable()`
    /// returns false for every Tag diff; the gate must not follow it.
    #[test]
    fn both_tag_states_gate() {
        for op in [
            tag::TagOp::ReferencedButUnregistered,
            tag::TagOp::RegisteredButUnreferenced,
        ] {
            let d = tag(op);
            assert!(!d.is_actionable());
            assert_eq!(d.drift_tier(), DriftTier::Gating);
        }
    }

    #[test]
    fn unchanged_tag_is_not_drift() {
        assert_eq!(tag(tag::TagOp::Unchanged).drift_tier(), DriftTier::None);
    }

    #[test]
    fn counts_split_changed_into_the_two_tiers() {
        let report_only = ResourceDiff::CustomAttribute(custom_attribute::CustomAttributeDiff {
            name: "trial_started_at".into(),
            op: custom_attribute::CustomAttributeOp::PresentInGitOnly,
            hints: vec![],
        });
        let in_sync = ResourceDiff::ContentBlock(content_block::ContentBlockDiff {
            name: "promo".into(),
            op: DiffOp::Unchanged,
            text_diff: None,
            orphan: false,
        });
        let summary = DiffSummary {
            diffs: vec![report_only, orphan_content_block(), in_sync],
        };
        assert_eq!(summary.changed_count(), 2);
        assert_eq!(summary.gating_drift_count(), 1);
        assert_eq!(summary.report_only_drift_count(), 1);
        assert_eq!(summary.in_sync_count(), 1);
    }
}
