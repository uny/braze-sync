//! Tag diff types and registry comparison.
//!
//! Tags are GitOps-only: Braze has no public REST API for managing them.
//! `diff` compares the local registry (`tags/registry.yaml`) against the
//! union of tag names referenced on local resources (content_blocks,
//! email_templates) and reports the symmetric drift:
//!
//! - `ReferencedButUnregistered`: a resource references this tag but it
//!   is not declared in the registry. Apply would fail at the Braze API
//!   layer with `400 Tags could not be found`. This is **actionable**
//!   for the operator (add to registry + create in Braze dashboard) and
//!   blocks `apply` pre-flight.
//! - `RegisteredButUnreferenced`: declared in the registry but no
//!   resource currently uses it. Informational — may be a deletion the
//!   operator hasn't pruned, or a tag staged for upcoming use.
//! - `Unchanged`: declared and referenced, in sync.
//!
//! `apply` cannot mutate tags directly, so no diff variant is "actionable"
//! in the apply-mutation sense. The diff is consumed by validate and by
//! `apply`'s pre-flight check.

use crate::resource::{Tag, TagRegistry};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct TagDiff {
    pub name: String,
    pub op: TagOp,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum TagOp {
    /// Referenced by at least one local resource but missing from the
    /// registry. Apply pre-flight will block on this.
    ReferencedButUnregistered,
    /// Declared in the registry but no local resource references it.
    RegisteredButUnreferenced,
    Unchanged,
}

impl TagDiff {
    pub fn has_changes(&self) -> bool {
        !matches!(self.op, TagOp::Unchanged)
    }
}

/// Compare a local registry against a set of tag names actually
/// referenced by local resources.
///
/// `referenced` is the union of `tags:` arrays across content_blocks,
/// email_templates, etc. `local` is the registry file. Either may be
/// `None` / empty.
pub fn diff(local: Option<&TagRegistry>, referenced: &BTreeSet<String>) -> Vec<TagDiff> {
    let registry_by_name: BTreeMap<&str, &Tag> = local
        .map(|r| {
            let mut map = BTreeMap::new();
            for t in &r.tags {
                if map.insert(t.name.as_str(), t).is_some() {
                    tracing::warn!(
                        name = t.name.as_str(),
                        "duplicate tag name in local registry; \
                         last entry wins (run `validate` to catch this)"
                    );
                }
            }
            map
        })
        .unwrap_or_default();

    let mut all_names: BTreeSet<&str> = BTreeSet::new();
    all_names.extend(registry_by_name.keys().copied());
    all_names.extend(referenced.iter().map(String::as_str));

    let mut diffs = Vec::new();
    for name in all_names {
        let in_registry = registry_by_name.contains_key(name);
        let is_referenced = referenced.contains(name);
        let op = match (in_registry, is_referenced) {
            (true, true) => TagOp::Unchanged,
            (true, false) => TagOp::RegisteredButUnreferenced,
            (false, true) => TagOp::ReferencedButUnregistered,
            (false, false) => unreachable!("name came from one of the two maps"),
        };
        diffs.push(TagDiff {
            name: name.to_string(),
            op,
            hints: Vec::new(),
        });
    }

    diffs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::Tag;

    fn registry(names: &[&str]) -> TagRegistry {
        TagRegistry {
            tags: names
                .iter()
                .map(|n| Tag {
                    name: (*n).to_string(),
                    description: None,
                })
                .collect(),
        }
    }

    fn referenced(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_diff_when_registry_matches_references() {
        let r = registry(&["a", "b"]);
        let used = referenced(&["a", "b"]);
        let diffs = diff(Some(&r), &used);
        assert_eq!(diffs.len(), 2);
        assert!(diffs.iter().all(|d| !d.has_changes()));
    }

    #[test]
    fn referenced_but_unregistered_tag_is_flagged() {
        let r = registry(&["a"]);
        let used = referenced(&["a", "missing"]);
        let diffs = diff(Some(&r), &used);
        let missing = diffs.iter().find(|d| d.name == "missing").unwrap();
        assert!(matches!(missing.op, TagOp::ReferencedButUnregistered));
    }

    #[test]
    fn registered_but_unreferenced_tag_is_flagged() {
        let r = registry(&["a", "orphan"]);
        let used = referenced(&["a"]);
        let diffs = diff(Some(&r), &used);
        let orphan = diffs.iter().find(|d| d.name == "orphan").unwrap();
        assert!(matches!(orphan.op, TagOp::RegisteredButUnreferenced));
    }

    #[test]
    fn missing_registry_treats_all_references_as_unregistered() {
        let used = referenced(&["a", "b"]);
        let diffs = diff(None, &used);
        assert_eq!(diffs.len(), 2);
        assert!(diffs
            .iter()
            .all(|d| matches!(d.op, TagOp::ReferencedButUnregistered)));
    }

    #[test]
    fn empty_when_neither_side_has_tags() {
        let diffs = diff(None, &referenced(&[]));
        assert!(diffs.is_empty());
    }

    #[test]
    fn diffs_are_sorted_by_name() {
        let r = registry(&["zebra", "apple"]);
        let used = referenced(&["mango"]);
        let diffs = diff(Some(&r), &used);
        let names: Vec<&str> = diffs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["apple", "mango", "zebra"]);
    }
}
