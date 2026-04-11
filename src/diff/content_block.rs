//! Content Block diff. See IMPLEMENTATION.md §11.3 / §11.6.
//!
//! Two things make this differ from Catalog Schema:
//!
//! 1. **No DELETE endpoint.** A content block that exists in Braze but
//!    not in Git is an *orphan*, not a `Removed`. The diff carries an
//!    explicit `orphan: bool` and the apply path can rename via
//!    `--archive-orphans` instead of pretending it can drop it.
//! 2. **Body comparison via `similar`.** When both sides exist and the
//!    bodies differ, we compute a [`TextDiffSummary`] (line additions /
//!    deletions) so the formatter can show "+5 -3" without dumping the
//!    full hunk.
//!
//! ### Why `state` is excluded from the syncable comparison
//!
//! IMPLEMENTATION.md §6.3 puts a `state: Active|Draft` enum on the
//! domain `ContentBlock`, but Braze's `/content_blocks/info` response
//! does not actually carry a state field. The braze client defaults
//! every fetched block to `Active`. If we compared whole-struct equality
//! (`l == r`), a local file with `state: draft` would diff as Modified
//! against any remote — and `apply` could not actually change the
//! remote because there is no API for it. That is the exact "infinite
//! drift" failure mode the §11.6 honest-orphan design is trying to
//! prevent for the no-DELETE case. Excluding `state` from the syncable
//! check keeps the local file free to carry that metadata for human
//! readers without producing false-positive diffs. Confirming whether
//! Braze ever exposes content block state is a Phase C E2E gate item
//! (PHASE_A_NOTES.md §6).

use crate::diff::DiffOp;
use crate::resource::ContentBlock;
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone)]
pub struct ContentBlockDiff {
    pub name: String,
    pub op: DiffOp<ContentBlock>,
    pub text_diff: Option<TextDiffSummary>,
    /// True when present in Braze but missing from Git. See §11.6.
    pub orphan: bool,
}

#[derive(Debug, Clone)]
pub struct TextDiffSummary {
    pub additions: usize,
    pub deletions: usize,
    pub unified_hunks: Vec<String>,
}

impl ContentBlockDiff {
    pub fn has_changes(&self) -> bool {
        self.op.is_change() || self.orphan
    }

    pub fn is_orphan(&self) -> bool {
        self.orphan
    }
}

/// Compute a per-name content block diff.
///
/// Returns `None` only when both sides are absent. The local side is
/// the desired state and the remote side is what Braze currently has.
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
        // local 不在 + remote 存在 → orphan (§11.6). The op stays
        // Unchanged because there is no DELETE API to call; the orphan
        // flag is what the apply path branches on.
        (None, Some(r)) => Some(ContentBlockDiff {
            name: r.name.clone(),
            op: DiffOp::Unchanged,
            text_diff: None,
            orphan: true,
        }),
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
/// Excludes `state` for the reason documented at the top of this file.
fn syncable_eq(a: &ContentBlock, b: &ContentBlock) -> bool {
    a.name == b.name && a.description == b.description && a.content == b.content && a.tags == b.tags
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
    let hunks = diff
        .unified_diff()
        .context_radius(3)
        .to_string()
        .lines()
        .map(String::from)
        .collect();
    TextDiffSummary {
        additions,
        deletions,
        unified_hunks: hunks,
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
    fn state_difference_alone_is_not_drift() {
        // The whole point of syncable_eq excluding state. Local says
        // Draft, remote (defaulted) says Active — and we treat the pair
        // as in-sync because braze-sync cannot actually push state.
        let mut l = cb("state", "body\n");
        let r = cb("state", "body\n");
        l.state = ContentBlockState::Draft;
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(!d.has_changes());
    }

    #[test]
    fn destructive_count_is_never_set_on_content_blocks() {
        // Sanity: ContentBlockDiff has no destructive concept (no DELETE
        // endpoint exists). The aggregate has_destructive in
        // diff/mod.rs returns false unconditionally for ContentBlock,
        // so we just confirm orphan + Modified both stay non-destructive.
        let r = cb("ghost", "x");
        let orphan = diff(None, Some(&r)).unwrap();
        assert!(!orphan.op.is_destructive());

        let l2 = cb("changed", "new");
        let r2 = cb("changed", "old");
        let modified = diff(Some(&l2), Some(&r2)).unwrap();
        assert!(!modified.op.is_destructive());
    }
}
