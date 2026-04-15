//! Custom Attribute diff types and registry comparison.
//!
//! The only mutation `apply` can perform is the deprecation flag toggle.

use crate::diff::opt_str_eq;
use crate::resource::{CustomAttribute, CustomAttributeRegistry};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct CustomAttributeDiff {
    pub name: String,
    pub op: CustomAttributeOp,
    /// Non-actionable notes surfaced in diff output (e.g. "description
    /// also differs", "type is stale"). These do NOT count as changes.
    pub hints: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum CustomAttributeOp {
    /// Present in Braze but missing from local registry. Action: prompt `export`.
    UnregisteredInGit,
    /// Present in local registry but not in Braze. Often a typo.
    PresentInGitOnly,
    /// `deprecated` flag changed. The only mutation `apply` actually performs.
    DeprecationToggled {
        from: bool,
        to: bool,
    },
    /// Only the description changed. No API to update it, so `apply` is a no-op.
    MetadataOnly,
    Unchanged,
}

impl CustomAttributeDiff {
    pub fn has_changes(&self) -> bool {
        !matches!(self.op, CustomAttributeOp::Unchanged)
    }

    /// Whether `apply` should consider this diff actionable — i.e. it
    /// must not be skipped by the "No changes to apply" early exit.
    ///
    /// - `DeprecationToggled` produces an API call.
    /// - `PresentInGitOnly` is included so it reaches
    ///   `check_for_unsupported_ops` and produces a clear rejection
    ///   error rather than being silently ignored.
    /// - `MetadataOnly` and `UnregisteredInGit` are informational drift
    ///   that `apply` cannot resolve (the fix is `export`, not `apply`).
    pub fn is_actionable(&self) -> bool {
        matches!(
            self.op,
            CustomAttributeOp::DeprecationToggled { .. } | CustomAttributeOp::PresentInGitOnly
        )
    }
}

/// Compare a local registry against a remote (Braze) attribute set and
/// produce one [`CustomAttributeDiff`] per attribute name across both
/// sides. Results are sorted by name.
///
/// Either side may be `None` (no local file yet, or no remote
/// attributes). When both are `None` the result is empty.
pub fn diff_registry(
    local: Option<&CustomAttributeRegistry>,
    remote: &[CustomAttribute],
) -> Vec<CustomAttributeDiff> {
    let local_by_name: BTreeMap<&str, &CustomAttribute> = local
        .map(|r| {
            let mut map = BTreeMap::new();
            for a in &r.attributes {
                if map.insert(a.name.as_str(), a).is_some() {
                    tracing::warn!(
                        name = a.name.as_str(),
                        "duplicate custom attribute name in local registry; \
                         last entry wins (run `validate` to catch this)"
                    );
                }
            }
            map
        })
        .unwrap_or_default();

    let remote_by_name: BTreeMap<&str, &CustomAttribute> =
        remote.iter().map(|a| (a.name.as_str(), a)).collect();

    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(local_by_name.keys());
    all_names.extend(remote_by_name.keys());

    let mut diffs = Vec::new();
    for name in all_names {
        let l = local_by_name.get(name);
        let r = remote_by_name.get(name);
        let (op, hints) = match (l, r) {
            (Some(local_attr), Some(remote_attr)) => diff_single_attribute(local_attr, remote_attr),
            (Some(_), None) => (CustomAttributeOp::PresentInGitOnly, Vec::new()),
            (None, Some(_)) => (CustomAttributeOp::UnregisteredInGit, Vec::new()),
            (None, None) => unreachable!("name came from one of the two maps"),
        };
        diffs.push(CustomAttributeDiff {
            name: name.to_string(),
            op,
            hints,
        });
    }

    diffs
}

/// Compare a single attribute present on both sides.
///
/// Priority order:
///   1. `deprecated` flag → `DeprecationToggled` (the only actionable mutation)
///   2. `description` text → `MetadataOnly`
///   3. `attribute_type` → `Unchanged` (Braze is authoritative; see below)
///
/// When both `deprecated` and `description` differ, only
/// `DeprecationToggled` is reported. This is by design: `apply` will
/// push the deprecation toggle, and the user should re-run `export`
/// afterwards to reconcile the description with Braze's state.
fn diff_single_attribute(
    local: &CustomAttribute,
    remote: &CustomAttribute,
) -> (CustomAttributeOp, Vec<String>) {
    let mut hints = Vec::new();

    if local.deprecated != remote.deprecated {
        if !opt_str_eq(&local.description, &remote.description) {
            hints.push("description also differs; will be reconciled on next export".into());
        }
        return (
            CustomAttributeOp::DeprecationToggled {
                from: remote.deprecated,
                to: local.deprecated,
            },
            hints,
        );
    }
    if !opt_str_eq(&local.description, &remote.description) {
        return (CustomAttributeOp::MetadataOnly, hints);
    }
    // attribute_type differences are treated as `Unchanged` — not
    // `MetadataOnly` — because the semantics differ: MetadataOnly means
    // "the user intentionally changed something that can't be pushed",
    // whereas a type mismatch means "the local registry is stale" (Braze
    // is the sole authority on types). The fix is always `export`, not
    // `apply`.
    if local.attribute_type != remote.attribute_type {
        hints.push(format!(
            "type mismatch: local {} vs Braze {} (run export to update)",
            local.attribute_type, remote.attribute_type,
        ));
    }
    (CustomAttributeOp::Unchanged, hints)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::CustomAttributeType;

    fn attr(name: &str, deprecated: bool, desc: Option<&str>) -> CustomAttribute {
        CustomAttribute {
            name: name.into(),
            attribute_type: CustomAttributeType::String,
            description: desc.map(Into::into),
            deprecated,
        }
    }

    #[test]
    fn both_sides_empty() {
        let diffs = diff_registry(None, &[]);
        assert!(diffs.is_empty());
    }

    #[test]
    fn local_only_attributes() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("foo", false, None)],
        };
        let diffs = diff_registry(Some(&registry), &[]);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].name, "foo");
        assert!(matches!(diffs[0].op, CustomAttributeOp::PresentInGitOnly));
    }

    #[test]
    fn remote_only_attributes() {
        let remote = vec![attr("bar", false, None)];
        let diffs = diff_registry(None, &remote);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].name, "bar");
        assert!(matches!(diffs[0].op, CustomAttributeOp::UnregisteredInGit));
    }

    #[test]
    fn duplicate_local_name_uses_last_entry() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("dup", true, None), attr("dup", false, None)],
        };
        let remote = vec![attr("dup", false, None)];
        let diffs = diff_registry(Some(&registry), &remote);
        assert_eq!(diffs.len(), 1);
        // Last entry (deprecated=false) wins → matches remote → Unchanged.
        assert!(matches!(diffs[0].op, CustomAttributeOp::Unchanged));
    }

    #[test]
    fn unchanged_attributes() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("x", false, Some("desc"))],
        };
        let remote = vec![attr("x", false, Some("desc"))];
        let diffs = diff_registry(Some(&registry), &remote);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0].op, CustomAttributeOp::Unchanged));
    }

    #[test]
    fn deprecation_toggled_local_deprecates() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("x", true, None)],
        };
        let remote = vec![attr("x", false, None)];
        let diffs = diff_registry(Some(&registry), &remote);
        assert_eq!(diffs.len(), 1);
        match &diffs[0].op {
            CustomAttributeOp::DeprecationToggled { from, to } => {
                assert!(!from);
                assert!(to);
            }
            other => panic!("expected DeprecationToggled, got {other:?}"),
        }
    }

    #[test]
    fn deprecation_toggled_local_reactivates() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("x", false, None)],
        };
        let remote = vec![attr("x", true, None)];
        let diffs = diff_registry(Some(&registry), &remote);
        match &diffs[0].op {
            CustomAttributeOp::DeprecationToggled { from, to } => {
                assert!(from);
                assert!(!to);
            }
            other => panic!("expected DeprecationToggled, got {other:?}"),
        }
    }

    #[test]
    fn metadata_only_description_changed() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("x", false, Some("new desc"))],
        };
        let remote = vec![attr("x", false, Some("old desc"))];
        let diffs = diff_registry(Some(&registry), &remote);
        assert!(matches!(diffs[0].op, CustomAttributeOp::MetadataOnly));
    }

    #[test]
    fn metadata_only_description_added() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("x", false, Some("added"))],
        };
        let remote = vec![attr("x", false, None)];
        let diffs = diff_registry(Some(&registry), &remote);
        assert!(matches!(diffs[0].op, CustomAttributeOp::MetadataOnly));
    }

    #[test]
    fn deprecation_takes_precedence_over_metadata() {
        let registry = CustomAttributeRegistry {
            attributes: vec![CustomAttribute {
                name: "x".into(),
                attribute_type: CustomAttributeType::String,
                description: Some("new desc".into()),
                deprecated: true,
            }],
        };
        let remote = vec![CustomAttribute {
            name: "x".into(),
            attribute_type: CustomAttributeType::String,
            description: Some("old desc".into()),
            deprecated: false,
        }];
        let diffs = diff_registry(Some(&registry), &remote);
        assert!(matches!(
            diffs[0].op,
            CustomAttributeOp::DeprecationToggled { .. }
        ));
    }

    #[test]
    fn mixed_operations_sorted_by_name() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("charlie", false, None), attr("alpha", true, None)],
        };
        let remote = vec![attr("alpha", false, None), attr("bravo", false, None)];
        let diffs = diff_registry(Some(&registry), &remote);
        assert_eq!(diffs.len(), 3);
        assert_eq!(diffs[0].name, "alpha");
        assert!(matches!(
            diffs[0].op,
            CustomAttributeOp::DeprecationToggled { .. }
        ));
        assert_eq!(diffs[1].name, "bravo");
        assert!(matches!(diffs[1].op, CustomAttributeOp::UnregisteredInGit));
        assert_eq!(diffs[2].name, "charlie");
        assert!(matches!(diffs[2].op, CustomAttributeOp::PresentInGitOnly));
    }

    #[test]
    fn type_difference_alone_is_unchanged() {
        let registry = CustomAttributeRegistry {
            attributes: vec![CustomAttribute {
                name: "x".into(),
                attribute_type: CustomAttributeType::Number,
                description: None,
                deprecated: false,
            }],
        };
        let remote = vec![CustomAttribute {
            name: "x".into(),
            attribute_type: CustomAttributeType::String,
            description: None,
            deprecated: false,
        }];
        let diffs = diff_registry(Some(&registry), &remote);
        assert!(matches!(diffs[0].op, CustomAttributeOp::Unchanged));
    }

    #[test]
    fn has_changes_correctly_classifies() {
        let unchanged = CustomAttributeDiff {
            name: "x".into(),
            op: CustomAttributeOp::Unchanged,
            hints: Vec::new(),
        };
        assert!(!unchanged.has_changes());

        let changed = CustomAttributeDiff {
            name: "x".into(),
            op: CustomAttributeOp::PresentInGitOnly,
            hints: Vec::new(),
        };
        assert!(changed.has_changes());
    }

    #[test]
    fn is_actionable_correctly_classifies() {
        let make = |op: CustomAttributeOp| CustomAttributeDiff {
            name: "x".into(),
            op,
            hints: Vec::new(),
        };
        assert!(make(CustomAttributeOp::DeprecationToggled { from: false, to: true }).is_actionable());
        assert!(make(CustomAttributeOp::PresentInGitOnly).is_actionable());
        assert!(!make(CustomAttributeOp::MetadataOnly).is_actionable());
        assert!(!make(CustomAttributeOp::UnregisteredInGit).is_actionable());
        assert!(!make(CustomAttributeOp::Unchanged).is_actionable());
    }

    #[test]
    fn deprecation_toggle_with_description_diff_adds_hint() {
        let registry = CustomAttributeRegistry {
            attributes: vec![CustomAttribute {
                name: "x".into(),
                attribute_type: CustomAttributeType::String,
                description: Some("local desc".into()),
                deprecated: true,
            }],
        };
        let remote = vec![CustomAttribute {
            name: "x".into(),
            attribute_type: CustomAttributeType::String,
            description: Some("remote desc".into()),
            deprecated: false,
        }];
        let diffs = diff_registry(Some(&registry), &remote);
        assert!(matches!(
            diffs[0].op,
            CustomAttributeOp::DeprecationToggled { .. }
        ));
        assert_eq!(diffs[0].hints.len(), 1);
        assert!(diffs[0].hints[0].contains("description"));
    }

    #[test]
    fn type_mismatch_adds_hint_but_stays_unchanged() {
        let registry = CustomAttributeRegistry {
            attributes: vec![CustomAttribute {
                name: "x".into(),
                attribute_type: CustomAttributeType::Number,
                description: None,
                deprecated: false,
            }],
        };
        let remote = vec![CustomAttribute {
            name: "x".into(),
            attribute_type: CustomAttributeType::String,
            description: None,
            deprecated: false,
        }];
        let diffs = diff_registry(Some(&registry), &remote);
        assert!(matches!(diffs[0].op, CustomAttributeOp::Unchanged));
        assert_eq!(diffs[0].hints.len(), 1);
        assert!(diffs[0].hints[0].contains("type mismatch"));
        // Verify snake_case format (Display), not Debug (Number/String).
        assert!(
            diffs[0].hints[0].contains("local number vs Braze string"),
            "hint should use snake_case: {}",
            diffs[0].hints[0]
        );
    }

    #[test]
    fn no_hints_when_fully_matching() {
        let registry = CustomAttributeRegistry {
            attributes: vec![attr("x", false, Some("desc"))],
        };
        let remote = vec![attr("x", false, Some("desc"))];
        let diffs = diff_registry(Some(&registry), &remote);
        assert!(diffs[0].hints.is_empty());
    }
}
