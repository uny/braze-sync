//! Apply-order computation for content blocks.
//!
//! ### Why this module exists
//!
//! Braze's `POST /content_blocks/create` validates the body at create time
//! and rejects the request when a `{{content_blocks.${other_block}}}`
//! include refers to a block that does not yet exist in the workspace.
//! Worse, the failure surfaces as **HTTP 500** with an opaque body, not
//! as a 4xx validation error — so a single forward reference halts the
//! whole apply pass and leaves earlier writes committed.
//!
//! Default alphabetical apply order is unaware of these cross-block
//! dependencies and any `A → B` reference where `A < B` alphabetically
//! breaks on a fresh workspace. This module parses references out of
//! the local Liquid bodies, builds a `referrer → target` graph over the
//! actionable diffs, and re-emits them in dependency order (targets
//! before referrers). Cycles are reported with named blocks before any
//! HTTP write fires; references that point outside the actionable set
//! (already-present blocks, or out-of-scope names) are treated as
//! no-ops — the referrer becomes a leaf for sort purposes.
//!
//! Pure module: no I/O, no Braze calls.

use crate::diff::{DiffOp, ResourceDiff};
use regex_lite::Regex;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

/// Match `{{content_blocks.${NAME}}}` with whitespace tolerance and an
/// optional `| id: '...'` filter. The id alias is not load-bearing for
/// the dependency graph — only `NAME` matters. Inside `${...}` we accept
/// any run of non-space, non-`}`, non-`|` characters so unusual but
/// valid Braze names (digits, hyphens) round-trip.
fn ref_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\{\{\s*content_blocks\.\$\{\s*([^\s}|]+)\s*\}(?:\s*\|[^}]*)?\s*\}\}")
            .expect("static regex")
    })
}

/// Names of content blocks referenced by `body` via the Liquid include
/// syntax. Order is source-order; duplicates are preserved (callers
/// dedupe as needed). Self-references are not filtered here — the
/// caller has the surrounding context to decide.
pub fn extract_block_references(body: &str) -> Vec<String> {
    ref_regex()
        .captures_iter(body)
        .map(|cap| cap[1].to_string())
        .collect()
}

/// A dependency cycle. The path is reported in the order encountered
/// during DFS, with the closing node repeated so callers can render
/// `A → B → C → A`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cycle {
    pub path: Vec<String>,
}

impl std::fmt::Display for Cycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.path.join(" → "))
    }
}

/// Topologically sort `nodes` by `edges` (referrer → targets). Targets
/// that are not in `nodes` are treated as no-op edges — the referrer
/// just becomes a leaf for sort purposes. Output is dependency-order:
/// targets before referrers. Stable across runs: ties broken by input
/// order of `nodes`, so the dry-run plan for the same repo state is
/// reproducible. Edge target lists are walked in given order; callers
/// who want deterministic cycle-path messages should pre-sort/dedup.
pub fn topo_sort<'a>(
    nodes: &'a [String],
    edges: &'a BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>, Cycle> {
    let in_set: BTreeSet<&'a str> = nodes.iter().map(String::as_str).collect();
    let mut visited: BTreeSet<&'a str> = BTreeSet::new();
    let mut on_stack: Vec<&'a str> = Vec::new();
    let mut on_stack_set: BTreeSet<&'a str> = BTreeSet::new();
    let mut out: Vec<&'a str> = Vec::with_capacity(nodes.len());

    for n in nodes {
        let n = n.as_str();
        if !visited.contains(n) {
            visit(
                n,
                edges,
                &in_set,
                &mut visited,
                &mut on_stack,
                &mut on_stack_set,
                &mut out,
            )?;
        }
    }
    Ok(out.into_iter().map(str::to_owned).collect())
}

fn visit<'a>(
    node: &'a str,
    edges: &'a BTreeMap<String, Vec<String>>,
    in_set: &BTreeSet<&'a str>,
    visited: &mut BTreeSet<&'a str>,
    on_stack: &mut Vec<&'a str>,
    on_stack_set: &mut BTreeSet<&'a str>,
    out: &mut Vec<&'a str>,
) -> Result<(), Cycle> {
    if on_stack_set.contains(node) {
        // Cycle: cut the prefix before the first occurrence and append
        // the closing node so the message reads `A → B → C → A`.
        let start = on_stack
            .iter()
            .position(|n| *n == node)
            .expect("on_stack and on_stack_set must stay in sync");
        let mut path: Vec<String> = on_stack[start..].iter().map(|s| (*s).to_owned()).collect();
        path.push(node.to_owned());
        return Err(Cycle { path });
    }
    if visited.contains(node) {
        return Ok(());
    }
    on_stack.push(node);
    on_stack_set.insert(node);

    if let Some(targets) = edges.get(node) {
        for t in targets {
            let t = t.as_str();
            if in_set.contains(t) {
                visit(t, edges, in_set, visited, on_stack, on_stack_set, out)?;
            }
        }
    }

    on_stack.pop();
    on_stack_set.remove(node);
    visited.insert(node);
    out.push(node);
    Ok(())
}

/// Reorder content_block diffs so that, for every actionable
/// (`Added`/`Modified`) diff with a `{{content_blocks.${target}}}`
/// reference whose `target` is also in the actionable set, `target`
/// appears earlier in the returned list than the referrer.
///
/// Non-actionable diffs (`Unchanged`, orphans) keep their relative
/// position among themselves but are emitted **after** the actionable
/// block — they don't fire writes, so apply order doesn't affect them
/// and putting them after the actionable group keeps the dry-run plan
/// readable: changes first, no-ops last.
pub fn reorder_content_block_diffs_by_dependency(
    diffs: Vec<ResourceDiff>,
) -> Result<Vec<ResourceDiff>, Cycle> {
    let mut others: Vec<ResourceDiff> = Vec::new();
    let mut inactionable: Vec<ResourceDiff> = Vec::new();
    let mut actionable_names: Vec<String> = Vec::new();
    let mut by_name: BTreeMap<String, ResourceDiff> = BTreeMap::new();
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for d in diffs {
        match d {
            ResourceDiff::ContentBlock(cb) => {
                let body: Option<&str> = if cb.orphan {
                    None
                } else {
                    match &cb.op {
                        DiffOp::Added(b) => Some(b.content.as_str()),
                        DiffOp::Modified { to, .. } => Some(to.content.as_str()),
                        _ => None,
                    }
                };
                match body {
                    Some(b) => {
                        let name = cb.name.clone();
                        // Self-references are a Braze-side problem, not
                        // an apply-order problem — drop them so they
                        // don't surface as false-positive cycles.
                        let mut refs: Vec<String> = extract_block_references(b)
                            .into_iter()
                            .filter(|t| *t != name)
                            .collect();
                        refs.sort();
                        refs.dedup();
                        if !refs.is_empty() {
                            edges.insert(name.clone(), refs);
                        }
                        actionable_names.push(name.clone());
                        by_name.insert(name, ResourceDiff::ContentBlock(cb));
                    }
                    None => inactionable.push(ResourceDiff::ContentBlock(cb)),
                }
            }
            other => others.push(other),
        }
    }

    let sorted_names = topo_sort(&actionable_names, &edges)?;

    let mut out = Vec::with_capacity(others.len() + sorted_names.len() + inactionable.len());
    // Non-content_block diffs keep their original relative position
    // before the content_block group, mirroring `apply::run`'s per-kind
    // iteration order. Inactionable blocks trail — apply order doesn't
    // affect them and trailing keeps the dry-run plan readable.
    out.extend(others);
    for n in &sorted_names {
        out.push(
            by_name
                .remove(n)
                .expect("topo_sort returns names from input set"),
        );
    }
    out.extend(inactionable);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::content_block::ContentBlockDiff;
    use crate::resource::{ContentBlock, ContentBlockState};

    fn cb(name: &str, body: &str) -> ContentBlock {
        ContentBlock {
            name: name.into(),
            description: None,
            content: body.into(),
            tags: vec![],
            state: ContentBlockState::Active,
        }
    }

    fn added(name: &str, body: &str) -> ResourceDiff {
        ResourceDiff::ContentBlock(ContentBlockDiff {
            name: name.into(),
            op: DiffOp::Added(cb(name, body)),
            text_diff: None,
            orphan: false,
        })
    }

    fn unchanged(name: &str) -> ResourceDiff {
        ResourceDiff::ContentBlock(ContentBlockDiff {
            name: name.into(),
            op: DiffOp::Unchanged,
            text_diff: None,
            orphan: false,
        })
    }

    fn order(diffs: &[ResourceDiff]) -> Vec<&str> {
        diffs
            .iter()
            .filter_map(|d| match d {
                ResourceDiff::ContentBlock(cb) => Some(cb.name.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn extracts_a_single_reference() {
        let body = "Hello {{content_blocks.${other_block} | id: 'cb1'}}!";
        assert_eq!(extract_block_references(body), vec!["other_block"]);
    }

    #[test]
    fn extracts_references_with_whitespace_variations() {
        let body = "
            {{content_blocks.${a}}}
            {{ content_blocks.${ b } }}
            {{content_blocks.${c} | id: 'x' }}
            {{ content_blocks.${d}  | id: 'y'}}
        ";
        assert_eq!(extract_block_references(body), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn extracts_multiple_references_in_one_body() {
        let body = "head {{content_blocks.${one}}} mid {{content_blocks.${two}}} tail";
        assert_eq!(extract_block_references(body), vec!["one", "two"]);
    }

    #[test]
    fn ignores_unrelated_liquid() {
        let body = "{{ user.${first_name} }} {{ campaign.${id} }}";
        assert!(extract_block_references(body).is_empty());
    }

    #[test]
    fn topo_sort_emits_targets_before_referrers() {
        // A → B (A references B). B must come first.
        let nodes = vec!["a".to_string(), "b".to_string()];
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        edges.insert("a".into(), vec!["b".into()]);
        let out = topo_sort(&nodes, &edges).unwrap();
        assert_eq!(out, vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn topo_sort_drops_edges_to_unknown_targets() {
        // A → B, but B isn't in the input set. A is treated as a leaf.
        let nodes = vec!["a".to_string()];
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        edges.insert("a".into(), vec!["b".into()]);
        let out = topo_sort(&nodes, &edges).unwrap();
        assert_eq!(out, vec!["a".to_string()]);
    }

    #[test]
    fn topo_sort_detects_cycle_and_names_it() {
        let nodes = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
        edges.insert("a".into(), vec!["b".into()]);
        edges.insert("b".into(), vec!["c".into()]);
        edges.insert("c".into(), vec!["a".into()]);
        let err = topo_sort(&nodes, &edges).unwrap_err();
        // Cycle path closes on itself.
        assert_eq!(err.path.first(), err.path.last());
        let s: BTreeSet<&str> = err.path.iter().map(String::as_str).collect();
        assert!(s.contains("a") && s.contains("b") && s.contains("c"));
    }

    #[test]
    fn reorder_puts_dependency_target_before_referrer() {
        // Spec example: A < B alphabetically, A references B → after
        // reorder, B is emitted before A.
        let diffs = vec![
            added("a_referrer", "see {{content_blocks.${b_target}}}"),
            added("b_target", "leaf"),
        ];
        let out = reorder_content_block_diffs_by_dependency(diffs).unwrap();
        assert_eq!(order(&out), vec!["b_target", "a_referrer"]);
    }

    #[test]
    fn reorder_keeps_independent_blocks_in_input_order() {
        let diffs = vec![
            added("alpha", "no refs"),
            added("bravo", "no refs"),
            added("charlie", "no refs"),
        ];
        let out = reorder_content_block_diffs_by_dependency(diffs).unwrap();
        assert_eq!(order(&out), vec!["alpha", "bravo", "charlie"]);
    }

    #[test]
    fn reorder_treats_reference_to_unchanged_block_as_leaf() {
        // The target is already in Braze (Unchanged here stands in for
        // "remote-known"), so the referrer can be emitted in its
        // natural slot — no error, no reorder needed.
        let diffs = vec![
            added("referrer", "see {{content_blocks.${already_there}}}"),
            unchanged("already_there"),
        ];
        let out = reorder_content_block_diffs_by_dependency(diffs).unwrap();
        let names = order(&out);
        // Actionable group first, non-actionable after.
        assert_eq!(names, vec!["referrer", "already_there"]);
    }

    #[test]
    fn reorder_reports_cycle_with_block_names() {
        let diffs = vec![
            added("a", "{{content_blocks.${b}}}"),
            added("b", "{{content_blocks.${a}}}"),
        ];
        let err = reorder_content_block_diffs_by_dependency(diffs).unwrap_err();
        let s: BTreeSet<&str> = err.path.iter().map(String::as_str).collect();
        assert!(s.contains("a") && s.contains("b"));
    }

    #[test]
    fn reorder_drops_self_reference_silently() {
        // A self-reference is a Braze-side problem (the body refers to
        // itself), not an apply-order problem. Don't surface it as a
        // false-positive cycle here.
        let diffs = vec![added("loner", "{{content_blocks.${loner}}}")];
        let out = reorder_content_block_diffs_by_dependency(diffs).unwrap();
        assert_eq!(order(&out), vec!["loner"]);
    }

    #[test]
    fn reorder_handles_diamond_correctly() {
        //   a
        //  / \
        // b   c
        //  \ /
        //   d
        // d must come first; b and c before a.
        let diffs = vec![
            added("a", "{{content_blocks.${b}}} and {{content_blocks.${c}}}"),
            added("b", "{{content_blocks.${d}}}"),
            added("c", "{{content_blocks.${d}}}"),
            added("d", "leaf"),
        ];
        let out = reorder_content_block_diffs_by_dependency(diffs).unwrap();
        let names = order(&out);
        let pos = |n: &str| names.iter().position(|x| *x == n).unwrap();
        assert!(pos("d") < pos("b"));
        assert!(pos("d") < pos("c"));
        assert!(pos("b") < pos("a"));
        assert!(pos("c") < pos("a"));
    }
}
