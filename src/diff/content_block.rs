//! Content Block diff.
//!
//! ### Why `state` is excluded from the syncable comparison
//!
//! Braze's `/content_blocks/info` response does not carry a state field
//! and the braze client defaults every fetched block to `Active`.
//! Comparing whole-struct equality would make any local file with
//! `state: draft` diff as Modified forever — the "infinite drift" mode
//! the orphan design is meant to prevent for resources with no DELETE
//! endpoint. Excluding `state` keeps the local file free to carry that
//! metadata for human readers without producing false-positive diffs.

use crate::diff::DiffOp;
use crate::resource::ContentBlock;
use similar::{ChangeTag, TextDiff};
use std::collections::BTreeMap;

/// Name → Braze `content_block_id`. Built during diff, consumed by
/// apply to translate per-name plan entries into the id the update
/// endpoint requires.
pub type ContentBlockIdIndex = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct ContentBlockDiff {
    pub name: String,
    pub op: DiffOp<ContentBlock>,
    pub text_diff: Option<TextDiffSummary>,
    /// True when present in Braze but missing from Git.
    pub orphan: bool,
}

#[derive(Debug, Clone)]
pub struct TextDiffSummary {
    pub additions: usize,
    pub deletions: usize,
}

impl ContentBlockDiff {
    pub fn has_changes(&self) -> bool {
        self.op.is_change() || self.orphan
    }

    pub fn is_orphan(&self) -> bool {
        self.orphan
    }

    /// Canonical constructor for the remote-only / orphan shape. No
    /// DELETE API → `op` stays `Unchanged`; callers branch on the
    /// `orphan` flag instead.
    pub fn orphan(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            op: DiffOp::Unchanged,
            text_diff: None,
            orphan: true,
        }
    }
}

/// Returns `None` only when both sides are absent. Local is desired
/// state, remote is current Braze state.
pub fn diff(
    local: Option<&ContentBlock>,
    remote: Option<&ContentBlock>,
) -> Option<ContentBlockDiff> {
    match (local, remote) {
        (None, None) => None,
        (Some(l), None) => Some(ContentBlockDiff {
            name: l.name.clone(),
            op: DiffOp::Added(l.clone()),
            text_diff: None,
            orphan: false,
        }),
        (None, Some(r)) => Some(ContentBlockDiff::orphan(&r.name)),
        (Some(l), Some(r)) => {
            if syncable_eq(l, r) {
                Some(ContentBlockDiff {
                    name: l.name.clone(),
                    op: DiffOp::Unchanged,
                    text_diff: None,
                    orphan: false,
                })
            } else {
                let text_diff = if l.content != r.content {
                    Some(compute_text_diff(&r.content, &l.content))
                } else {
                    None
                };
                Some(ContentBlockDiff {
                    name: l.name.clone(),
                    op: DiffOp::Modified {
                        from: r.clone(),
                        to: l.clone(),
                    },
                    text_diff,
                    orphan: false,
                })
            }
        }
    }
}

/// Equality for the fields braze-sync can actually push to Braze.
/// Excludes `state` — see the module docs. Tags are compared as a
/// multiset because Braze's content block APIs don't document tag-order
/// stability across `/info` fetches; an order-sensitive comparison would
/// surface a reorder as Modified, let apply push the local order back,
/// and potentially flip on the next diff — the same infinite-drift
/// failure mode the `state` exclusion exists to prevent.
fn syncable_eq(a: &ContentBlock, b: &ContentBlock) -> bool {
    a.name == b.name
        && a.description == b.description
        && a.content == b.content
        && tags_eq_unordered(&a.tags, &b.tags)
}

/// Multiset equality: same length + same elements after sort. Keeps
/// duplicate-awareness (unlike a set) so `["a","a"]` vs `["a"]` still
/// surfaces as a real change on the off chance Braze ever returns
/// duplicated tags.
fn tags_eq_unordered(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a: Vec<&str> = a.iter().map(String::as_str).collect();
    let mut b: Vec<&str> = b.iter().map(String::as_str).collect();
    a.sort_unstable();
    b.sort_unstable();
    a == b
}

fn compute_text_diff(from: &str, to: &str) -> TextDiffSummary {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ContentBlockState;

    fn cb(name: &str, body: &str) -> ContentBlock {
        ContentBlock {
            name: name.into(),
            description: Some(format!("{name} desc")),
            content: body.into(),
            tags: vec!["tag".into()],
            state: ContentBlockState::Active,
        }
    }

    #[test]
    fn both_absent_returns_none() {
        assert!(diff(None, None).is_none());
    }

    #[test]
    fn local_only_is_added() {
        let l = cb("promo", "Hello");
        let d = diff(Some(&l), None).unwrap();
        assert!(matches!(d.op, DiffOp::Added(_)));
        assert!(!d.orphan);
        assert!(d.has_changes());
    }

    #[test]
    fn remote_only_is_orphan_not_removed() {
        let r = cb("legacy", "old body");
        let d = diff(None, Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(d.orphan);
        assert!(d.is_orphan());
        assert!(d.has_changes());
        assert!(d.text_diff.is_none());
    }

    #[test]
    fn equal_blocks_are_unchanged() {
        let l = cb("same", "body\n");
        let r = cb("same", "body\n");
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(!d.orphan);
        assert!(!d.has_changes());
        assert!(d.text_diff.is_none());
    }

    #[test]
    fn body_difference_is_modified_with_text_diff() {
        let l = cb("body_drift", "line a\nline b\nline c\n");
        let r = cb("body_drift", "line a\nold b\nline c\n");
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        let td = d.text_diff.expect("text diff present for body changes");
        assert_eq!(td.additions, 1);
        assert_eq!(td.deletions, 1);
    }

    #[test]
    fn description_only_change_is_modified_without_text_diff() {
        let mut l = cb("desc_drift", "same body\n");
        let mut r = cb("desc_drift", "same body\n");
        l.description = Some("new".into());
        r.description = Some("old".into());
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        // Body identical → no text diff to show.
        assert!(d.text_diff.is_none());
    }

    #[test]
    fn tags_change_is_modified_without_text_diff() {
        let mut l = cb("tag_drift", "body\n");
        let mut r = cb("tag_drift", "body\n");
        l.tags = vec!["a".into(), "b".into()];
        r.tags = vec!["a".into()];
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        assert!(d.text_diff.is_none());
    }

    #[test]
    fn tag_reorder_alone_is_not_drift() {
        // Braze's content block APIs don't document tag-order stability
        // across /info fetches, so a reorder must surface as Unchanged.
        // Otherwise apply would push local order back and the diff could
        // flip forever — same infinite-drift mode as the state exclusion.
        let mut l = cb("tag_reorder", "body\n");
        let mut r = cb("tag_reorder", "body\n");
        l.tags = vec!["alpha".into(), "beta".into(), "gamma".into()];
        r.tags = vec!["gamma".into(), "alpha".into(), "beta".into()];
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged), "got {:?}", d.op);
        assert!(!d.has_changes());
    }

    #[test]
    fn tag_multiset_difference_with_same_length_is_drift() {
        // Regression guard: sort+eq must not collapse same-length vecs
        // with different element sets into "equal".
        let mut l = cb("tag_set", "body\n");
        let mut r = cb("tag_set", "body\n");
        l.tags = vec!["a".into(), "b".into()];
        r.tags = vec!["a".into(), "c".into()];
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
    }

    #[test]
    fn state_difference_alone_is_not_drift() {
        let mut l = cb("state", "body\n");
        let r = cb("state", "body\n");
        l.state = ContentBlockState::Draft;
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(!d.has_changes());
    }

    #[test]
    fn destructive_count_is_never_set_on_content_blocks() {
        let r = cb("ghost", "x");
        let orphan = diff(None, Some(&r)).unwrap();
        assert!(!orphan.op.is_destructive());

        let l2 = cb("changed", "new");
        let r2 = cb("changed", "old");
        let modified = diff(Some(&l2), Some(&r2)).unwrap();
        assert!(!modified.op.is_destructive());
    }
}
