//! Plan file schema and matching for `diff --plan-out` / `apply --plan`.
//!
//! The plan file freezes the set of actionable ops produced by `diff` so
//! that `apply` can refuse to run when the live Braze state has drifted
//! since the plan was generated (Terraform-style plan/apply lock).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;

use crate::diff::custom_attribute::CustomAttributeOp;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::resource::ResourceKind;

pub const CURRENT_PLAN_VERSION: u32 = 1;

/// Warn at apply time when the saved plan is older than this.
pub const STALE_PLAN_WARN_THRESHOLD: chrono::TimeDelta = chrono::TimeDelta::hours(24);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanFile {
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub braze_sync_version: String,
    pub scope: PlanScope,
    pub ops: Vec<PlanOp>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanScope {
    pub environment: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlanOp {
    pub kind: ResourceKind,
    pub name: String,
    pub op: PlanOpType,
}

/// The coarse op classification used for plan locking. Field-level
/// payloads are deliberately excluded so the plan file stays safe to
/// publish as a CI artifact and so the apply-time comparison tolerates
/// benign content edits made between plan and apply.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PlanOpType {
    Add,
    Modify,
    DestructiveDelete,
    Orphan,
    Deprecate,
    Reactivate,
}

impl PlanFile {
    pub fn from_summary(
        summary: &DiffSummary,
        environment: impl Into<String>,
        resource: Option<ResourceKind>,
        name: Option<String>,
    ) -> Self {
        Self {
            version: CURRENT_PLAN_VERSION,
            generated_at: Utc::now(),
            braze_sync_version: env!("CARGO_PKG_VERSION").to_string(),
            scope: PlanScope {
                environment: environment.into(),
                resource,
                name,
            },
            ops: collect_ops(summary),
        }
    }

    pub fn read_from(path: &Path) -> std::io::Result<Self> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Compare saved ops against `fresh` (multiset over `(kind, name, op_type)`,
    /// order-independent). Builds each side's set once.
    pub fn diff_ops(&self, fresh: &[PlanOp]) -> PlanOpsDiff {
        let saved_set: BTreeSet<&PlanOp> = self.ops.iter().collect();
        let fresh_set: BTreeSet<&PlanOp> = fresh.iter().collect();
        let missing = self
            .ops
            .iter()
            .filter(|op| !fresh_set.contains(op))
            .cloned()
            .collect();
        let extra = fresh
            .iter()
            .filter(|op| !saved_set.contains(op))
            .cloned()
            .collect();
        PlanOpsDiff { missing, extra }
    }
}

/// Result of comparing a saved plan's ops against a freshly-computed list.
#[derive(Debug, Default)]
pub struct PlanOpsDiff {
    /// In saved plan but not in fresh (resolved or absorbed remotely).
    pub missing: Vec<PlanOp>,
    /// In fresh but not in saved plan (new drift since plan).
    pub extra: Vec<PlanOp>,
}

impl PlanOpsDiff {
    pub fn is_match(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty()
    }
}

/// Convert a `DiffSummary` into the coarse plan-op list. Skips non-actionable
/// diffs (Tag drift, Custom Attribute metadata-only, Unchanged) so the
/// plan-lock vocabulary matches the set of operations apply can actually
/// perform.
pub fn collect_ops(summary: &DiffSummary) -> Vec<PlanOp> {
    let mut out = Vec::new();
    for diff in &summary.diffs {
        let Some(op) = classify(diff) else { continue };
        out.push(PlanOp {
            kind: diff.kind(),
            name: diff.name().to_string(),
            op,
        });
    }
    out.sort();
    out
}

fn classify(diff: &ResourceDiff) -> Option<PlanOpType> {
    match diff {
        ResourceDiff::CatalogSchema(d) => match &d.op {
            DiffOp::Added(_) => Some(PlanOpType::Add),
            DiffOp::Removed(_) => Some(PlanOpType::DestructiveDelete),
            DiffOp::Modified { .. } => Some(PlanOpType::Modify),
            DiffOp::Unchanged => {
                // Field-level edits with the catalog itself "unchanged".
                if d.field_diffs.iter().any(|f| f.is_destructive()) {
                    Some(PlanOpType::DestructiveDelete)
                } else if d.field_diffs.iter().any(|f| f.is_change()) {
                    Some(PlanOpType::Modify)
                } else {
                    None
                }
            }
        },
        ResourceDiff::ContentBlock(d) => classify_orphanable(d.orphan, &d.op),
        ResourceDiff::EmailTemplate(d) => classify_orphanable(d.orphan, &d.op),
        ResourceDiff::CustomAttribute(d) => match &d.op {
            CustomAttributeOp::DeprecationToggled { to: true, .. } => Some(PlanOpType::Deprecate),
            CustomAttributeOp::DeprecationToggled { to: false, .. } => Some(PlanOpType::Reactivate),
            _ => None,
        },
        ResourceDiff::Tag(_) => None,
    }
}

fn classify_orphanable<T>(orphan: bool, op: &DiffOp<T>) -> Option<PlanOpType> {
    if orphan {
        return Some(PlanOpType::Orphan);
    }
    match op {
        DiffOp::Added(_) => Some(PlanOpType::Add),
        DiffOp::Modified { .. } => Some(PlanOpType::Modify),
        DiffOp::Removed(_) | DiffOp::Unchanged => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(kind: ResourceKind, name: &str, op: PlanOpType) -> PlanOp {
        PlanOp {
            kind,
            name: name.to_string(),
            op,
        }
    }

    #[test]
    fn ops_match_is_order_independent() {
        let plan = PlanFile {
            version: 1,
            generated_at: Utc::now(),
            braze_sync_version: "test".into(),
            scope: PlanScope {
                environment: "dev".into(),
                resource: None,
                name: None,
            },
            ops: vec![
                op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
                op(ResourceKind::CatalogSchema, "x", PlanOpType::Add),
            ],
        };
        let fresh = vec![
            op(ResourceKind::CatalogSchema, "x", PlanOpType::Add),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
        ];
        assert!(plan.diff_ops(&fresh).is_match());
    }

    #[test]
    fn ops_mismatch_when_op_kind_changes() {
        let plan = PlanFile {
            version: 1,
            generated_at: Utc::now(),
            braze_sync_version: "test".into(),
            scope: PlanScope {
                environment: "dev".into(),
                resource: None,
                name: None,
            },
            ops: vec![op(ResourceKind::ContentBlock, "a", PlanOpType::Modify)],
        };
        let fresh = vec![op(ResourceKind::ContentBlock, "a", PlanOpType::Orphan)];
        let diff = plan.diff_ops(&fresh);
        assert!(!diff.is_match());
        assert_eq!(diff.missing.len(), 1);
        assert_eq!(diff.extra.len(), 1);
    }

    #[test]
    fn round_trip_json() {
        let plan = PlanFile {
            version: 1,
            generated_at: "2026-05-18T12:34:56Z".parse().unwrap(),
            braze_sync_version: "0.12.0".into(),
            scope: PlanScope {
                environment: "dev".into(),
                resource: Some(ResourceKind::ContentBlock),
                name: None,
            },
            ops: vec![op(ResourceKind::ContentBlock, "hero", PlanOpType::Add)],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let round: PlanFile = serde_json::from_str(&json).unwrap();
        assert!(plan.diff_ops(&round.ops).is_match());
        assert_eq!(round.scope, plan.scope);
    }
}
