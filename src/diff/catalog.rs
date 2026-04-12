//! Catalog Schema and Catalog Items diff. See IMPLEMENTATION.md §11.1 / §11.2.

use crate::diff::DiffOp;
use crate::resource::{Catalog, CatalogField};
use std::collections::HashMap;

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
            field_diffs: vec![],
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

#[derive(Debug, Clone)]
pub struct CatalogItemsDiff {
    pub catalog_name: String,
    pub added_ids: Vec<String>,
    pub modified_ids: Vec<String>,
    pub removed_ids: Vec<String>,
    pub unchanged_count: usize,
}

impl CatalogItemsDiff {
    pub fn has_changes(&self) -> bool {
        !self.added_ids.is_empty() || !self.modified_ids.is_empty() || !self.removed_ids.is_empty()
    }

    pub fn has_destructive(&self) -> bool {
        !self.removed_ids.is_empty()
    }
}

/// Diff catalog items by comparing their content hashes. Only the hash
/// maps are needed — the actual row data is not loaded or compared.
///
/// `catalog_name` is passed explicitly so the caller controls which name
/// appears in the diff — the local map may be empty when the catalog
/// only exists remotely.
///
/// Output id lists are sorted for deterministic display and test assertions.
pub fn diff_items(
    catalog_name: &str,
    local_hashes: &HashMap<String, String>,
    remote_hashes: &HashMap<String, String>,
) -> CatalogItemsDiff {
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut removed = Vec::new();
    let mut unchanged: usize = 0;

    for (id, lhash) in local_hashes {
        match remote_hashes.get(id) {
            None => added.push(id.clone()),
            Some(rhash) if rhash != lhash => modified.push(id.clone()),
            Some(_) => unchanged += 1,
        }
    }
    for id in remote_hashes.keys() {
        if !local_hashes.contains_key(id) {
            removed.push(id.clone());
        }
    }

    added.sort();
    modified.sort();
    removed.sort();

    CatalogItemsDiff {
        catalog_name: catalog_name.to_string(),
        added_ids: added,
        modified_ids: modified,
        removed_ids: removed,
        unchanged_count: unchanged,
    }
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
        let l = cat("c", vec![field("id", CatalogFieldType::String)]);
        let d = diff_schema(Some(&l), None).unwrap();
        assert!(matches!(d.op, DiffOp::Added(_)));
        assert!(d.has_changes());
        assert!(!d.has_destructive());
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

    #[test]
    fn items_diff_stub_destructive_when_removed() {
        let d = CatalogItemsDiff {
            catalog_name: "c".into(),
            added_ids: vec![],
            modified_ids: vec![],
            removed_ids: vec!["x".into()],
            unchanged_count: 0,
        };
        assert!(d.has_changes());
        assert!(d.has_destructive());
    }

    // =============================================================
    // diff_items tests
    // =============================================================

    fn hashes(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, h)| (id.to_string(), h.to_string()))
            .collect()
    }

    #[test]
    fn diff_items_both_empty() {
        let d = diff_items("c", &hashes(&[]), &hashes(&[]));
        assert!(!d.has_changes());
        assert_eq!(d.unchanged_count, 0);
    }

    #[test]
    fn diff_items_all_added() {
        let local = hashes(&[("a", "h1"), ("b", "h2")]);
        let remote = hashes(&[]);
        let d = diff_items("c", &local, &remote);
        assert_eq!(d.added_ids, vec!["a", "b"]);
        assert!(d.modified_ids.is_empty());
        assert!(d.removed_ids.is_empty());
        assert_eq!(d.unchanged_count, 0);
    }

    #[test]
    fn diff_items_all_removed() {
        let local = hashes(&[]);
        let remote = hashes(&[("a", "h1"), ("b", "h2")]);
        let d = diff_items("c", &local, &remote);
        assert!(d.added_ids.is_empty());
        assert!(d.modified_ids.is_empty());
        assert_eq!(d.removed_ids, vec!["a", "b"]);
        assert!(d.has_destructive());
    }

    #[test]
    fn diff_items_all_unchanged() {
        let local = hashes(&[("a", "h1"), ("b", "h2")]);
        let remote = hashes(&[("a", "h1"), ("b", "h2")]);
        let d = diff_items("c", &local, &remote);
        assert!(!d.has_changes());
        assert_eq!(d.unchanged_count, 2);
    }

    #[test]
    fn diff_items_mixed() {
        let local = hashes(&[("a", "h1"), ("b", "h2_new"), ("d", "h4")]);
        let remote = hashes(&[("a", "h1"), ("b", "h2_old"), ("c", "h3")]);
        let d = diff_items("c", &local, &remote);
        assert_eq!(d.added_ids, vec!["d"]);
        assert_eq!(d.modified_ids, vec!["b"]);
        assert_eq!(d.removed_ids, vec!["c"]);
        assert_eq!(d.unchanged_count, 1);
    }

    #[test]
    fn diff_items_ids_are_sorted() {
        let local = hashes(&[("z", "h"), ("a", "h"), ("m", "h")]);
        let remote = hashes(&[]);
        let d = diff_items("c", &local, &remote);
        assert_eq!(d.added_ids, vec!["a", "m", "z"]);
    }

    #[test]
    fn diff_items_uses_explicit_catalog_name() {
        let local = hashes(&[]);
        let remote = hashes(&[("a", "h1")]);
        let d = diff_items("remote_only", &local, &remote);
        assert_eq!(d.catalog_name, "remote_only");
        assert_eq!(d.removed_ids, vec!["a"]);
    }
}
