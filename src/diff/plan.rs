//! Plan file schema and matching for `diff --plan-out` / `apply --plan`.
//!
//! # Contract
//!
//! A saved plan authorizes exactly this:
//!
//! > Apply the *current* local intent, provided that (a) the set of
//! > operations still has the shape the plan recorded, and (b) the
//! > remote side of every op that would overwrite a remote body is still
//! > what `diff` observed when the plan was generated.
//!
//! Both halves are enforced: (a) by the op multiset comparison in
//! [`PlanFile::diff_ops`], (b) by the [`RemotePrecondition`] carried on
//! each op and checked against a freshly-fetched remote at apply time.
//!
//! `Deprecate` / `Reactivate` are covered by (a) alone — they write a
//! boolean whose expected prior value the op direction already states, so
//! a remote toggle removes the op from the fresh diff rather than
//! surviving it with a stale precondition. See
//! [`PlanOpType::requires_precondition`].
//!
//! # What this does not promise
//!
//! - **It is not concurrency control.** The precondition is checked
//!   against apply's own fetch, then the write happens. As of 2026-09
//!   Braze's REST API offers no `If-Match` or expected-state token on
//!   any endpoint braze-sync writes to, so a lost update remains
//!   possible inside that window.
//! - **It does not freeze the change set.** Local edits between plan and
//!   apply are still applied as long as the op shapes match; the plan
//!   binds the remote preconditions, not the bytes that get written.
//! - **It does not detect identity replacement.** A remote object
//!   replaced by identical content under a different Braze ID projects to
//!   the same digest.
//! - **It does not cover catalog items.** Deleting a catalog deletes its
//!   items, which braze-sync never fetches; the schema digest says
//!   nothing about rows added since the plan.
//! - **`scope.environment` is a config name, not a workspace identity.**
//!   Repointing an environment at a different Braze workspace is not
//!   visible here.
//! - **A digest is not confidentiality.** Predictable content can be
//!   guessed and confirmed. It is small enough to publish as a CI
//!   artifact, which is why the plan carries digests rather than payloads.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::diff::custom_attribute::CustomAttributeOp;
use crate::diff::{digest, DiffOp, DiffSummary, ResourceDiff};
use crate::resource::ResourceKind;

/// Bumped whenever the op vocabulary or a digest projection changes.
/// Older plans are rejected rather than migrated: a plan whose meaning
/// this binary cannot reproduce is not evidence of anything.
pub const CURRENT_PLAN_VERSION: u32 = 2;

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

/// The state the remote side of an op must still be in for the plan to
/// authorize it.
///
/// Digests cover the surface `apply` would overwrite — see
/// [`crate::diff::digest`]. Read-only fields are deliberately outside the
/// projection: a remote edit to one cannot be clobbered, so it is not a
/// precondition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state", content = "digest")]
pub enum RemotePrecondition {
    /// The resource must still not exist remotely. Carried by `Add`.
    Absent,
    /// The remote object must still project to this blake3 hex digest.
    /// Carried by `Modify` and `DestructiveDelete`.
    Digest(String),
}

/// Length of the hex form of a blake3 digest, which is what
/// [`crate::diff::digest`] emits.
const DIGEST_HEX_LEN: usize = 64;

/// Whether a plan-supplied string is shaped like one of our digests.
/// The plan file is operator-supplied input, so the *shape* is checked
/// rather than assumed — see [`PlanFile::validate`].
fn is_digest_hex(s: &str) -> bool {
    s.len() == DIGEST_HEX_LEN && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl RemotePrecondition {
    /// Short human-readable form for drift reporting.
    ///
    /// Truncates by *character*, not by byte: `validate` rejects a
    /// malformed digest before this can be reached, but this is a public
    /// method on operator-supplied data and must not be one edit away
    /// from panicking on a split codepoint.
    pub fn describe(&self) -> String {
        match self {
            Self::Absent => "absent".to_string(),
            Self::Digest(d) => format!("digest {}…", d.chars().take(12).collect::<String>()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanOp {
    pub kind: ResourceKind,
    pub name: String,
    pub op: PlanOpType,
    /// `None` only for ops that write nothing to the resource's remote
    /// body — see [`PlanOpType::requires_precondition`]. Never "we could
    /// not compute one": a v2 plan missing a required precondition is
    /// rejected as malformed by [`PlanFile::validate`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<RemotePrecondition>,
}

impl PlanOp {
    /// The pairing key for [`PlanFile::diff_ops`]. Deliberately excludes
    /// `precondition`: ops are matched by shape first, then their
    /// preconditions are compared, so a drifted remote reads as "this op
    /// changed underneath you" rather than as an unrelated op appearing
    /// and another disappearing.
    fn shape(&self) -> (ResourceKind, &str, PlanOpType) {
        (self.kind, self.name.as_str(), self.op)
    }
}

/// The coarse op classification used for plan locking. Field-level
/// payloads are deliberately excluded so the plan file stays safe to
/// publish as a CI artifact and so the apply-time comparison tolerates
/// benign *local* edits made between plan and apply. Remote drift is
/// caught by [`RemotePrecondition`] instead, which needs only a digest.
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

impl PlanOpType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Modify => "modify",
            Self::DestructiveDelete => "destructive_delete",
            Self::Orphan => "orphan",
            Self::Deprecate => "deprecate",
            Self::Reactivate => "reactivate",
        }
    }

    /// Whether a v2 plan op of this type must carry a precondition.
    ///
    /// `Orphan` is report-only and its remote body is never even fetched
    /// (`cli::diff` issues `/info` only for names present on both sides).
    /// `Deprecate` / `Reactivate` write a boolean whose expected prior
    /// value is implied by the op direction itself.
    pub fn requires_precondition(self) -> bool {
        match self {
            Self::Add | Self::Modify | Self::DestructiveDelete => true,
            Self::Orphan | Self::Deprecate | Self::Reactivate => false,
        }
    }
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

    /// Compare saved ops against `fresh` as a multiset over
    /// `(kind, name, op_type)`, order-independent, then compare the
    /// preconditions of the pairs that matched.
    ///
    /// `collect_ops` is invariant-by-design unique per `(kind, name)`
    /// today, but we use a merge walk rather than a set so duplicates
    /// would surface as drift instead of being silently collapsed.
    pub fn diff_ops(&self, fresh: &[PlanOp]) -> PlanOpsDiff {
        let mut saved: Vec<&PlanOp> = self.ops.iter().collect();
        let mut fresh_sorted: Vec<&PlanOp> = fresh.iter().collect();
        saved.sort_by(|a, b| a.shape().cmp(&b.shape()));
        fresh_sorted.sort_by(|a, b| a.shape().cmp(&b.shape()));
        let (mut i, mut j) = (0, 0);
        let mut missing = Vec::new();
        let mut extra = Vec::new();
        let mut precondition_drift = Vec::new();
        while i < saved.len() && j < fresh_sorted.len() {
            match saved[i].shape().cmp(&fresh_sorted[j].shape()) {
                std::cmp::Ordering::Less => {
                    missing.push(saved[i].clone());
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    extra.push(fresh_sorted[j].clone());
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    // Equal shapes imply equal op types, and both
                    // `validate` (saved) and `classify` (fresh) pin the
                    // precondition kind to the op type — so either both
                    // sides carry one or neither does.
                    debug_assert_eq!(
                        saved[i].precondition.is_some(),
                        fresh_sorted[j].precondition.is_some(),
                        "precondition presence must follow the op type",
                    );
                    if let (Some(expected), Some(found)) =
                        (&saved[i].precondition, &fresh_sorted[j].precondition)
                    {
                        if expected != found {
                            precondition_drift.push(PreconditionDrift {
                                op: saved[i].clone(),
                                expected: expected.clone(),
                                found: found.clone(),
                            });
                        }
                    }
                    i += 1;
                    j += 1;
                }
            }
        }
        missing.extend(saved[i..].iter().map(|&op| op.clone()));
        extra.extend(fresh_sorted[j..].iter().map(|&op| op.clone()));
        PlanOpsDiff {
            missing,
            extra,
            precondition_drift,
        }
    }

    /// Reject a plan this binary cannot check.
    ///
    /// A missing precondition on an op that writes to a remote body is
    /// treated as a malformed plan, never as "skip the comparison for
    /// this op" — silently degrading to the pre-v2 shape-only check is
    /// exactly the failure mode v2 exists to remove.
    ///
    /// A digest that is not one of ours is rejected here for the same
    /// reason, and because everything downstream — the comparison, the
    /// drift report — is entitled to assume the shape this checks.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut problems = Vec::new();
        for op in &self.ops {
            let expected = match (op.op.requires_precondition(), op.op) {
                (false, _) => None,
                (true, PlanOpType::Add) => Some("absent"),
                (true, _) => Some("a digest"),
            };
            match (expected, &op.precondition) {
                (None, None) => {}
                (Some("absent"), Some(RemotePrecondition::Absent)) => {}
                (Some("a digest"), Some(RemotePrecondition::Digest(d))) if is_digest_hex(d) => {}
                (Some("a digest"), Some(RemotePrecondition::Digest(_))) => problems.push(format!(
                    "{} {}: op `{}` carries a malformed digest \
                     (expected {DIGEST_HEX_LEN} lowercase hex characters)",
                    op.kind.as_str(),
                    op.name,
                    op.op.as_str(),
                )),
                (None, Some(_)) => problems.push(format!(
                    "{} {}: op `{}` must not carry a remote precondition",
                    op.kind.as_str(),
                    op.name,
                    op.op.as_str(),
                )),
                (Some(want), found) => problems.push(format!(
                    "{} {}: op `{}` requires {} precondition, found {}",
                    op.kind.as_str(),
                    op.name,
                    op.op.as_str(),
                    want,
                    match found {
                        None => "none".to_string(),
                        Some(RemotePrecondition::Absent) => "absent".to_string(),
                        Some(RemotePrecondition::Digest(_)) => "a digest".to_string(),
                    },
                )),
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}

/// A shape-matched op pair whose remote precondition no longer holds:
/// the live Braze state moved between plan and apply.
#[derive(Debug, Clone)]
pub struct PreconditionDrift {
    pub op: PlanOp,
    pub expected: RemotePrecondition,
    pub found: RemotePrecondition,
}

/// Result of comparing a saved plan's ops against a freshly-computed list.
#[derive(Debug, Default)]
pub struct PlanOpsDiff {
    /// In saved plan but not in fresh (resolved or absorbed remotely).
    pub missing: Vec<PlanOp>,
    /// In fresh but not in saved plan (new drift since plan).
    pub extra: Vec<PlanOp>,
    /// Same op shape, but the remote is no longer what the plan observed.
    pub precondition_drift: Vec<PreconditionDrift>,
}

impl PlanOpsDiff {
    pub fn is_match(&self) -> bool {
        self.missing.is_empty() && self.extra.is_empty() && self.precondition_drift.is_empty()
    }
}

/// Convert a `DiffSummary` into the plan-op list. Skips non-actionable
/// diffs (Tag drift, Custom Attribute metadata-only, Unchanged) so the
/// plan-lock vocabulary matches the set of operations apply can actually
/// perform.
pub fn collect_ops(summary: &DiffSummary) -> Vec<PlanOp> {
    let mut out = Vec::new();
    for diff in &summary.diffs {
        let Some((op, precondition)) = classify(diff) else {
            continue;
        };
        out.push(PlanOp {
            kind: diff.kind(),
            name: diff.name().to_string(),
            op,
            precondition,
        });
    }
    out.sort_by(|a, b| a.shape().cmp(&b.shape()));
    out
}

/// Classify one diff into its plan op and the remote state that op
/// presupposes.
///
/// The precondition always comes from the `from` (remote) side. The `to`
/// side is local intent, which the plan deliberately does not bind — see
/// the module contract.
fn classify(diff: &ResourceDiff) -> Option<(PlanOpType, Option<RemotePrecondition>)> {
    match diff {
        ResourceDiff::CatalogSchema(d) => match &d.op {
            DiffOp::Added(_) => Some((PlanOpType::Add, Some(RemotePrecondition::Absent))),
            DiffOp::Removed(r) => {
                Some((PlanOpType::DestructiveDelete, digest_of(r, digest::catalog)))
            }
            // A field removal rewrites the schema destructively even
            // though the catalog itself is `Modified`. Classify on what
            // apply will actually do, so the plan's vocabulary and the
            // `--allow-destructive` gate agree.
            DiffOp::Modified { from, .. } => {
                let op = if d.field_diffs.iter().any(|f| f.is_destructive()) {
                    PlanOpType::DestructiveDelete
                } else {
                    PlanOpType::Modify
                };
                Some((op, digest_of(from, digest::catalog)))
            }
            // `diff_schema` bases the top-level op solely on field-level
            // changes, so `Unchanged` here means there are none.
            DiffOp::Unchanged => None,
        },
        ResourceDiff::ContentBlock(d) => {
            classify_orphanable(d.orphan, &d.op, digest::content_block)
        }
        ResourceDiff::EmailTemplate(d) => {
            classify_orphanable(d.orphan, &d.op, digest::email_template)
        }
        ResourceDiff::CustomAttribute(d) => match &d.op {
            // The baseline these presuppose is a boolean the op
            // direction already states; a digest would add nothing.
            CustomAttributeOp::DeprecationToggled { to: true, .. } => {
                Some((PlanOpType::Deprecate, None))
            }
            CustomAttributeOp::DeprecationToggled { to: false, .. } => {
                Some((PlanOpType::Reactivate, None))
            }
            _ => None,
        },
        ResourceDiff::Tag(_) => None,
    }
}

fn digest_of<T>(remote: &T, project: fn(&T) -> String) -> Option<RemotePrecondition> {
    Some(RemotePrecondition::Digest(project(remote)))
}

fn classify_orphanable<T>(
    orphan: bool,
    op: &DiffOp<T>,
    project: fn(&T) -> String,
) -> Option<(PlanOpType, Option<RemotePrecondition>)> {
    if orphan {
        // Report-only, and the remote body is never fetched for an
        // orphan — there is nothing to take a precondition on.
        return Some((PlanOpType::Orphan, None));
    }
    match op {
        DiffOp::Added(_) => Some((PlanOpType::Add, Some(RemotePrecondition::Absent))),
        DiffOp::Modified { from, .. } => Some((PlanOpType::Modify, digest_of(from, project))),
        DiffOp::Removed(_) | DiffOp::Unchanged => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::catalog::CatalogSchemaDiff;
    use crate::diff::content_block::ContentBlockDiff;
    use crate::resource::{
        Catalog, CatalogField, CatalogFieldType, ContentBlock, ContentBlockState,
    };

    /// A distinct, *well-formed* digest per name. `validate` checks the
    /// hex shape, so a readable placeholder like `digest-of-hero` would
    /// make every fixture below a malformed plan.
    fn digest_like(name: &str) -> String {
        blake3::hash(name.as_bytes()).to_hex().to_string()
    }

    fn op(kind: ResourceKind, name: &str, op: PlanOpType) -> PlanOp {
        let precondition = match op {
            PlanOpType::Add => Some(RemotePrecondition::Absent),
            PlanOpType::Modify | PlanOpType::DestructiveDelete => {
                Some(RemotePrecondition::Digest(digest_like(name)))
            }
            _ => None,
        };
        PlanOp {
            kind,
            name: name.to_string(),
            op,
            precondition,
        }
    }

    fn plan_with(ops: Vec<PlanOp>) -> PlanFile {
        PlanFile {
            version: CURRENT_PLAN_VERSION,
            generated_at: Utc::now(),
            braze_sync_version: "test".into(),
            scope: PlanScope {
                environment: "dev".into(),
                resource: None,
                name: None,
            },
            ops,
        }
    }

    #[test]
    fn ops_match_is_order_independent() {
        let plan = plan_with(vec![
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::CatalogSchema, "x", PlanOpType::Add),
        ]);
        let fresh = vec![
            op(ResourceKind::CatalogSchema, "x", PlanOpType::Add),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
        ];
        assert!(plan.diff_ops(&fresh).is_match());
    }

    #[test]
    fn ops_mismatch_when_op_kind_changes() {
        let plan = plan_with(vec![op(
            ResourceKind::ContentBlock,
            "a",
            PlanOpType::Modify,
        )]);
        let fresh = vec![op(ResourceKind::ContentBlock, "a", PlanOpType::Orphan)];
        let diff = plan.diff_ops(&fresh);
        assert!(!diff.is_match());
        assert_eq!(diff.missing.len(), 1);
        assert_eq!(diff.extra.len(), 1);
        assert!(diff.precondition_drift.is_empty());
    }

    #[test]
    fn duplicate_ops_are_treated_as_multiset() {
        // Hand-crafted: two identical ops on either side should match,
        // but `n` on one side and `n+1` on the other should surface as
        // a single extra (set semantics would collapse to a match).
        let plan = plan_with(vec![
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
        ]);
        let fresh_dup = vec![
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
        ];
        assert!(plan.diff_ops(&fresh_dup).is_match());

        let fresh_one = vec![op(ResourceKind::ContentBlock, "a", PlanOpType::Modify)];
        let diff = plan.diff_ops(&fresh_one);
        assert!(!diff.is_match(), "should detect missing duplicate");
        assert_eq!(diff.missing.len(), 1);
        assert!(diff.extra.is_empty());
    }

    #[test]
    fn round_trip_json() {
        let plan = PlanFile {
            version: CURRENT_PLAN_VERSION,
            generated_at: "2026-05-18T12:34:56Z".parse().unwrap(),
            braze_sync_version: "0.12.0".into(),
            scope: PlanScope {
                environment: "dev".into(),
                resource: Some(ResourceKind::ContentBlock),
                name: None,
            },
            ops: vec![
                op(ResourceKind::ContentBlock, "hero", PlanOpType::Add),
                op(ResourceKind::ContentBlock, "promo", PlanOpType::Modify),
                op(ResourceKind::ContentBlock, "stale", PlanOpType::Orphan),
            ],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let round: PlanFile = serde_json::from_str(&json).unwrap();
        assert!(plan.diff_ops(&round.ops).is_match());
        assert_eq!(round.scope, plan.scope);
        assert_eq!(round.ops, plan.ops);
    }

    // -------------------------------------------------------------
    // Remote preconditions
    // -------------------------------------------------------------

    #[test]
    fn same_op_shape_with_changed_remote_is_drift() {
        // The #100 scenario in miniature: shape unchanged, remote moved.
        let plan = plan_with(vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: Some(RemotePrecondition::Digest("aaa".into())),
        }]);
        let fresh = vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: Some(RemotePrecondition::Digest("bbb".into())),
        }];
        let diff = plan.diff_ops(&fresh);
        assert!(!diff.is_match());
        assert!(diff.missing.is_empty(), "shape is unchanged");
        assert!(diff.extra.is_empty(), "shape is unchanged");
        assert_eq!(diff.precondition_drift.len(), 1);
    }

    #[test]
    fn validate_rejects_missing_digest() {
        let plan = plan_with(vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: None,
        }]);
        let problems = plan.validate().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("requires a digest"), "{problems:?}");
    }

    #[test]
    fn validate_rejects_wrong_precondition_kind() {
        let plan = plan_with(vec![
            PlanOp {
                kind: ResourceKind::CatalogSchema,
                name: "c".into(),
                op: PlanOpType::Add,
                precondition: Some(RemotePrecondition::Digest("x".into())),
            },
            PlanOp {
                kind: ResourceKind::ContentBlock,
                name: "orphaned".into(),
                op: PlanOpType::Orphan,
                precondition: Some(RemotePrecondition::Absent),
            },
        ]);
        let problems = plan.validate().unwrap_err();
        assert_eq!(problems.len(), 2);
        assert!(problems[0].contains("requires absent"), "{problems:?}");
        assert!(problems[1].contains("must not carry"), "{problems:?}");
    }

    #[test]
    fn validate_rejects_a_malformed_digest() {
        // Not hex — and byte 12 lands inside the '€', which is what
        // `describe` used to slice straight through.
        let plan = plan_with(vec![PlanOp {
            kind: ResourceKind::ContentBlock,
            name: "hero".into(),
            op: PlanOpType::Modify,
            precondition: Some(RemotePrecondition::Digest("aaaaaaaaaaa€zz".into())),
        }]);
        let problems = plan.validate().unwrap_err();
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("malformed digest"), "{problems:?}");
    }

    #[test]
    fn describe_truncates_by_character_not_byte() {
        // `validate` rejects this shape before `describe` can see it;
        // `describe` is public, so it must not panic on it regardless.
        let p = RemotePrecondition::Digest("aaaaaaaaaaa€zz".into());
        assert_eq!(p.describe(), "digest aaaaaaaaaaa€…");
        assert_eq!(
            RemotePrecondition::Digest(digest_like("hero")).describe(),
            format!("digest {}…", &digest_like("hero")[..12]),
        );
    }

    #[test]
    fn validate_accepts_a_well_formed_plan() {
        let plan = plan_with(vec![
            op(ResourceKind::CatalogSchema, "c", PlanOpType::Add),
            op(ResourceKind::ContentBlock, "a", PlanOpType::Modify),
            op(ResourceKind::ContentBlock, "b", PlanOpType::Orphan),
            op(ResourceKind::CustomAttribute, "d", PlanOpType::Deprecate),
            op(
                ResourceKind::CatalogSchema,
                "e",
                PlanOpType::DestructiveDelete,
            ),
        ]);
        assert!(plan.validate().is_ok());
    }

    // -------------------------------------------------------------
    // classify
    // -------------------------------------------------------------

    fn field(name: &str, field_type: CatalogFieldType) -> CatalogField {
        CatalogField {
            name: name.into(),
            field_type,
        }
    }

    fn catalog(name: &str, fields: Vec<CatalogField>) -> Catalog {
        Catalog {
            name: name.into(),
            description: None,
            fields,
        }
    }

    #[test]
    fn catalog_field_removal_classifies_as_destructive() {
        let remote = catalog(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("old", CatalogFieldType::Number),
            ],
        );
        let local = catalog("c", vec![field("id", CatalogFieldType::String)]);
        let d = crate::diff::catalog::diff_schema(Some(&local), Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::CatalogSchema(d)).unwrap();
        assert_eq!(op, PlanOpType::DestructiveDelete);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::catalog(&remote))),
        );
    }

    #[test]
    fn catalog_field_addition_classifies_as_modify_with_remote_digest() {
        let remote = catalog("c", vec![field("id", CatalogFieldType::String)]);
        let local = catalog(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("new", CatalogFieldType::Number),
            ],
        );
        let d = crate::diff::catalog::diff_schema(Some(&local), Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::CatalogSchema(d)).unwrap();
        assert_eq!(op, PlanOpType::Modify);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::catalog(&remote))),
            "precondition must digest the remote, not the local, side",
        );
    }

    #[test]
    fn catalog_with_no_field_diffs_is_not_a_plan_op() {
        let c = catalog("c", vec![field("id", CatalogFieldType::String)]);
        let mut local = c.clone();
        local.description = Some("description-only edit".into());
        let d = crate::diff::catalog::diff_schema(Some(&local), Some(&c)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(classify(&ResourceDiff::CatalogSchema(d)).is_none());
    }

    #[test]
    fn content_block_modify_digests_the_remote_side() {
        let remote = ContentBlock {
            name: "hero".into(),
            description: None,
            content: "remote".into(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        let mut local = remote.clone();
        local.content = "local".into();
        let d = crate::diff::content_block::diff(Some(&local), Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::ContentBlock(d)).unwrap();
        assert_eq!(op, PlanOpType::Modify);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::content_block(&remote))),
        );
        assert_ne!(
            precondition,
            Some(RemotePrecondition::Digest(digest::content_block(&local))),
        );
    }

    #[test]
    fn orphan_carries_no_precondition() {
        let d = ContentBlockDiff::orphan("gone");
        let (op, precondition) = classify(&ResourceDiff::ContentBlock(d)).unwrap();
        assert_eq!(op, PlanOpType::Orphan);
        assert_eq!(precondition, None);
    }

    #[test]
    fn removed_catalog_digests_the_remote_resource() {
        let remote = catalog("gone", vec![field("id", CatalogFieldType::String)]);
        let d: CatalogSchemaDiff = crate::diff::catalog::diff_schema(None, Some(&remote)).unwrap();
        let (op, precondition) = classify(&ResourceDiff::CatalogSchema(d)).unwrap();
        assert_eq!(op, PlanOpType::DestructiveDelete);
        assert_eq!(
            precondition,
            Some(RemotePrecondition::Digest(digest::catalog(&remote))),
        );
    }
}
