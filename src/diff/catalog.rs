//! Catalog Schema diff.

use crate::diff::DiffOp;
use crate::resource::{Catalog, CatalogField};

#[derive(Debug, Clone)]
pub struct CatalogSchemaDiff {
    pub name: String,
    pub op: DiffOp<Catalog>,
    pub field_diffs: Vec<DiffOp<CatalogField>>,
}

impl CatalogSchemaDiff {
    pub fn has_changes(&self) -> bool {
        self.op.is_change() || self.field_diffs.iter().any(|d| d.is_change())
    }

    pub fn has_destructive(&self) -> bool {
        self.op.is_destructive() || self.field_diffs.iter().any(|d| d.is_destructive())
    }
}

/// Diff a catalog schema between local intent and remote (Braze) state.
///
/// Returns `None` only when both sides are absent. The local side is treated
/// as the "to" / desired state and the remote as the "from".
pub fn diff_schema(local: Option<&Catalog>, remote: Option<&Catalog>) -> Option<CatalogSchemaDiff> {
    match (local, remote) {
        (None, None) => None,
        (Some(l), None) => Some(CatalogSchemaDiff {
            name: l.name.clone(),
            op: DiffOp::Added(l.clone()),
            // Surfaced so diff formatters can list the new fields under
            // the "+ new catalog" line; the apply path uses `op` instead.
            field_diffs: l.fields.iter().map(|f| DiffOp::Added(f.clone())).collect(),
        }),
        (None, Some(r)) => Some(CatalogSchemaDiff {
            name: r.name.clone(),
            op: DiffOp::Removed(r.clone()),
            field_diffs: vec![],
        }),
        (Some(l), Some(r)) => {
            let field_diffs = diff_fields(&l.fields, &r.fields);
            // Base the top-level op solely on field-level changes.
            // Description-only differences are not actionable in v0.1.0
            // (no endpoint to update catalog descriptions), so treating
            // them as Modified would show "1 changed" with no detail
            // lines and "Applied 0 change(s)" — confusing for users.
            let op = if field_diffs.is_empty() {
                DiffOp::Unchanged
            } else {
                DiffOp::Modified {
                    from: r.clone(),
                    to: l.clone(),
                }
            };
            Some(CatalogSchemaDiff {
                name: l.name.clone(),
                op,
                field_diffs,
            })
        }
    }
}

/// Field-level diff. `Unchanged` field-pairs are *not* recorded in the
/// output to keep diff summaries quiet.
///
/// Output ordering: Added and Modified ops come first (sorted by field
/// name via BTreeMap iteration), followed by Removed ops (also sorted
/// by field name). This is deterministic across runs and ensures
/// `apply` processes additions before removals — the safer direction.
fn diff_fields(local: &[CatalogField], remote: &[CatalogField]) -> Vec<DiffOp<CatalogField>> {
    use std::collections::BTreeMap;
    let l: BTreeMap<&String, &CatalogField> = local.iter().map(|f| (&f.name, f)).collect();
    let r: BTreeMap<&String, &CatalogField> = remote.iter().map(|f| (&f.name, f)).collect();

    let mut ops = Vec::new();
    for (name, lf) in &l {
        match r.get(name) {
            None => ops.push(DiffOp::Added((*lf).clone())),
            Some(rf) if rf != lf => ops.push(DiffOp::Modified {
                from: (*rf).clone(),
                to: (*lf).clone(),
            }),
            Some(_) => {} // Unchanged: omit from output
        }
    }
    for (name, rf) in &r {
        if !l.contains_key(name) {
            ops.push(DiffOp::Removed((*rf).clone()));
        }
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::CatalogFieldType;

    fn field(name: &str, t: CatalogFieldType) -> CatalogField {
        CatalogField {
            name: name.into(),
            field_type: t,
        }
    }

    fn cat(name: &str, fields: Vec<CatalogField>) -> Catalog {
        Catalog {
            name: name.into(),
            description: None,
            fields,
        }
    }

    #[test]
    fn both_absent_returns_none() {
        assert!(diff_schema(None, None).is_none());
    }

    #[test]
    fn local_only_is_added() {
        let l = cat(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("score", CatalogFieldType::Number),
            ],
        );
        let d = diff_schema(Some(&l), None).unwrap();
        assert!(matches!(d.op, DiffOp::Added(_)));
        assert!(d.has_changes());
        assert!(!d.has_destructive());
        assert_eq!(d.field_diffs.len(), 2);
        assert!(d.field_diffs.iter().all(|f| matches!(f, DiffOp::Added(_))));
    }

    #[test]
    fn remote_only_is_removed_and_destructive() {
        let r = cat("c", vec![field("id", CatalogFieldType::String)]);
        let d = diff_schema(None, Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Removed(_)));
        assert!(d.has_changes());
        assert!(d.has_destructive());
    }

    #[test]
    fn equal_catalogs_are_unchanged() {
        let l = cat("c", vec![field("id", CatalogFieldType::String)]);
        let r = cat("c", vec![field("id", CatalogFieldType::String)]);
        let d = diff_schema(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(d.field_diffs.is_empty());
        assert!(!d.has_changes());
        assert!(!d.has_destructive());
    }

    #[test]
    fn added_field_is_non_destructive() {
        let l = cat(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("score", CatalogFieldType::Number),
            ],
        );
        let r = cat("c", vec![field("id", CatalogFieldType::String)]);
        let d = diff_schema(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        assert_eq!(d.field_diffs.len(), 1);
        assert!(matches!(d.field_diffs[0], DiffOp::Added(_)));
        assert!(d.has_changes());
        assert!(!d.has_destructive());
    }

    #[test]
    fn removed_field_is_destructive() {
        let l = cat("c", vec![field("id", CatalogFieldType::String)]);
        let r = cat(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("legacy", CatalogFieldType::String),
            ],
        );
        let d = diff_schema(Some(&l), Some(&r)).unwrap();
        assert_eq!(d.field_diffs.len(), 1);
        assert!(matches!(d.field_diffs[0], DiffOp::Removed(_)));
        assert!(d.has_destructive());
    }

    #[test]
    fn type_change_is_modified_field() {
        let l = cat("c", vec![field("v", CatalogFieldType::Number)]);
        let r = cat("c", vec![field("v", CatalogFieldType::String)]);
        let d = diff_schema(Some(&l), Some(&r)).unwrap();
        assert_eq!(d.field_diffs.len(), 1);
        assert!(matches!(d.field_diffs[0], DiffOp::Modified { .. }));
        assert!(d.has_changes());
        // Type change is not a deletion → not destructive at the field op layer.
        assert!(!d.has_destructive());
    }

    #[test]
    fn unchanged_fields_are_not_recorded() {
        let l = cat(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("score", CatalogFieldType::Number),
            ],
        );
        let r = cat(
            "c",
            vec![
                field("id", CatalogFieldType::String),
                field("score", CatalogFieldType::Number),
            ],
        );
        let d = diff_schema(Some(&l), Some(&r)).unwrap();
        assert!(d.field_diffs.is_empty());
    }

    #[test]
    fn field_order_difference_is_not_drift() {
        let l = cat(
            "c",
            vec![
                field("a", CatalogFieldType::String),
                field("b", CatalogFieldType::Number),
            ],
        );
        let r = cat(
            "c",
            vec![
                field("b", CatalogFieldType::Number),
                field("a", CatalogFieldType::String),
            ],
        );
        let d = diff_schema(Some(&l), Some(&r)).unwrap();
        // Normalized comparison makes field order irrelevant at both the
        // top-level op and the field-diff layer.
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(d.field_diffs.is_empty());
        assert!(!d.has_changes());
    }

    #[test]
    fn description_only_difference_is_not_drift() {
        let l = Catalog {
            name: "c".into(),
            description: Some("local description".into()),
            fields: vec![field("id", CatalogFieldType::String)],
        };
        let r = Catalog {
            name: "c".into(),
            description: Some("remote description".into()),
            fields: vec![field("id", CatalogFieldType::String)],
        };
        let d = diff_schema(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(d.field_diffs.is_empty());
        assert!(!d.has_changes());
    }

    // -----------------------------------------------------------
    // digest <=> "diff_fields is empty", for two catalogs of the SAME
    // name — the digest also covers `name`, which `diff_fields` never
    // sees, and `diff_ops` pairs ops by name before comparing digests.
    // Catalog has no syncable_eq; the diff's notion of "unchanged" is an
    // empty field diff, and the plan's remote precondition must agree
    // with exactly that. See src/diff/digest.rs.
    // -----------------------------------------------------------

    mod digest_equivalence {
        use super::*;
        use crate::diff::digest;
        use crate::resource::Catalog;

        fn cat(fields: Vec<CatalogField>) -> Catalog {
            Catalog {
                name: "products".into(),
                description: None,
                fields,
            }
        }

        fn field(name: &str, field_type: CatalogFieldType) -> CatalogField {
            CatalogField {
                name: name.into(),
                field_type,
            }
        }

        fn base() -> Catalog {
            cat(vec![
                field("id", CatalogFieldType::String),
                field("price", CatalogFieldType::Number),
            ])
        }

        #[track_caller]
        fn assert_agree(a: &Catalog, b: &Catalog) {
            assert_eq!(
                diff_fields(&a.fields, &b.fields).is_empty(),
                digest::catalog(a) == digest::catalog(b),
                "digest and diff_fields disagree on {a:?} vs {b:?}",
            );
        }

        #[test]
        fn identical_agree() {
            assert_agree(&base(), &base());
        }

        #[test]
        fn field_order_agrees() {
            let mut b = base();
            b.fields.reverse();
            assert!(diff_fields(&base().fields, &b.fields).is_empty());
            assert_agree(&base(), &b);
        }

        #[test]
        fn description_is_ignored_by_both() {
            let mut b = base();
            b.description = Some("edited remotely".into());
            assert_agree(&base(), &b);
        }

        /// Compile-time guard: adding a field to `Catalog` or
        /// `CatalogField` breaks this destructure, which is the prompt to
        /// decide whether it belongs in `diff_fields` *and* in the
        /// projection. The variant list below is hand-written and cannot
        /// notice a field neither side looks at.
        #[test]
        fn variant_list_covers_every_field() {
            let Catalog {
                name: _,
                description: _,
                fields: _,
            } = base();
            let CatalogField {
                name: _,
                field_type: _,
            } = field("id", CatalogFieldType::String);
        }

        #[test]
        fn field_changes_disagree_in_both() {
            let variants = vec![
                cat(vec![field("id", CatalogFieldType::String)]),
                cat(vec![
                    field("id", CatalogFieldType::String),
                    field("price", CatalogFieldType::String),
                ]),
                cat(vec![
                    field("id", CatalogFieldType::String),
                    field("price", CatalogFieldType::Number),
                    field("sku", CatalogFieldType::String),
                ]),
                cat(vec![
                    field("id", CatalogFieldType::String),
                    field("cost", CatalogFieldType::Number),
                ]),
            ];
            for b in variants {
                assert!(
                    !diff_fields(&base().fields, &b.fields).is_empty(),
                    "expected a real change: {b:?}",
                );
                assert_agree(&base(), &b);
            }
        }
    }
}
