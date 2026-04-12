//! Email Template diff.
//!
//! ### Why `description` is excluded from the syncable comparison
//!
//! Braze's `/templates/email/info` returns `description` but the
//! create and update endpoints cannot set it. Including it in
//! `syncable_eq` would make any local file with `description: "..."` diff
//! as Modified forever — the same "infinite drift" mode the Content Block
//! `state` exclusion prevents. See PHASE_B1_NOTES.md §3 / §6.

use crate::diff::{compute_text_diff, opt_str_eq, tags_eq_unordered, DiffOp, TextDiffSummary};
use crate::resource::EmailTemplate;
use std::collections::BTreeMap;

/// Name → Braze `email_template_id`. Built during diff, consumed by
/// apply to translate per-name plan entries into the id the update
/// endpoint requires.
pub type EmailTemplateIdIndex = BTreeMap<String, String>;

#[derive(Debug, Clone)]
pub struct EmailTemplateDiff {
    pub name: String,
    pub op: DiffOp<EmailTemplate>,
    pub subject_changed: bool,
    pub body_html_diff: Option<TextDiffSummary>,
    pub body_plaintext_diff: Option<TextDiffSummary>,
    pub metadata_changed: bool,
    /// True when present in Braze but missing from Git.
    pub orphan: bool,
}

impl EmailTemplateDiff {
    pub fn has_changes(&self) -> bool {
        self.op.is_change()
            || self.subject_changed
            || self.body_html_diff.is_some()
            || self.body_plaintext_diff.is_some()
            || self.metadata_changed
            || self.orphan
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
            subject_changed: false,
            body_html_diff: None,
            body_plaintext_diff: None,
            metadata_changed: false,
            orphan: true,
        }
    }
}

/// Returns `None` only when both sides are absent. Local is desired
/// state, remote is current Braze state.
pub fn diff(
    local: Option<&EmailTemplate>,
    remote: Option<&EmailTemplate>,
) -> Option<EmailTemplateDiff> {
    match (local, remote) {
        (None, None) => None,
        (Some(l), None) => Some(EmailTemplateDiff {
            name: l.name.clone(),
            op: DiffOp::Added(l.clone()),
            subject_changed: false,
            body_html_diff: None,
            body_plaintext_diff: None,
            metadata_changed: false,
            orphan: false,
        }),
        (None, Some(r)) => Some(EmailTemplateDiff::orphan(&r.name)),
        (Some(l), Some(r)) => {
            if syncable_eq(l, r) {
                Some(EmailTemplateDiff {
                    name: l.name.clone(),
                    op: DiffOp::Unchanged,
                    subject_changed: false,
                    body_html_diff: None,
                    body_plaintext_diff: None,
                    metadata_changed: false,
                    orphan: false,
                })
            } else {
                let subject_changed = l.subject != r.subject;
                let body_html_diff = if l.body_html != r.body_html {
                    Some(compute_text_diff(&r.body_html, &l.body_html))
                } else {
                    None
                };
                let body_plaintext_diff = if l.body_plaintext != r.body_plaintext {
                    Some(compute_text_diff(&r.body_plaintext, &l.body_plaintext))
                } else {
                    None
                };
                let metadata_changed = !metadata_eq(l, r);
                Some(EmailTemplateDiff {
                    name: l.name.clone(),
                    op: DiffOp::Modified {
                        from: r.clone(),
                        to: l.clone(),
                    },
                    subject_changed,
                    body_html_diff,
                    body_plaintext_diff,
                    metadata_changed,
                    orphan: false,
                })
            }
        }
    }
}

/// Equality for the fields braze-sync can actually push to Braze.
/// Excludes `description` — see the module docs. Tags are compared as a
/// multiset because Braze's APIs don't document tag-order stability.
fn syncable_eq(a: &EmailTemplate, b: &EmailTemplate) -> bool {
    a.name == b.name
        && a.subject == b.subject
        && a.body_html == b.body_html
        && a.body_plaintext == b.body_plaintext
        && opt_str_eq(&a.preheader, &b.preheader)
        && a.should_inline_css == b.should_inline_css
        && tags_eq_unordered(&a.tags, &b.tags)
    // description excluded (read-only, like ContentBlock state)
}

/// Metadata equality (everything except subject, body_html, body_plaintext,
/// and description). Used to set the `metadata_changed` flag.
fn metadata_eq(a: &EmailTemplate, b: &EmailTemplate) -> bool {
    opt_str_eq(&a.preheader, &b.preheader)
        && a.should_inline_css == b.should_inline_css
        && tags_eq_unordered(&a.tags, &b.tags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn et(name: &str, html: &str) -> EmailTemplate {
        EmailTemplate {
            name: name.into(),
            subject: format!("Subject {name}"),
            body_html: html.into(),
            body_plaintext: format!("plain {name}"),
            description: Some(format!("{name} desc")),
            preheader: Some("preview".into()),
            should_inline_css: Some(true),
            tags: vec!["tag".into()],
        }
    }

    #[test]
    fn both_absent_returns_none() {
        assert!(diff(None, None).is_none());
    }

    #[test]
    fn local_only_is_added() {
        let l = et("welcome", "<p>Hi</p>");
        let d = diff(Some(&l), None).unwrap();
        assert!(matches!(d.op, DiffOp::Added(_)));
        assert!(!d.orphan);
        assert!(d.has_changes());
    }

    #[test]
    fn remote_only_is_orphan_not_removed() {
        let r = et("legacy", "<p>old</p>");
        let d = diff(None, Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(d.orphan);
        assert!(d.is_orphan());
        assert!(d.has_changes());
    }

    #[test]
    fn equal_templates_are_unchanged() {
        let l = et("same", "<p>body</p>\n");
        let r = et("same", "<p>body</p>\n");
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged));
        assert!(!d.orphan);
        assert!(!d.has_changes());
    }

    #[test]
    fn subject_change_is_modified() {
        let mut l = et("sub", "<p>body</p>\n");
        let r = et("sub", "<p>body</p>\n");
        l.subject = "New subject".into();
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        assert!(d.subject_changed);
        assert!(d.body_html_diff.is_none());
        assert!(d.body_plaintext_diff.is_none());
    }

    #[test]
    fn body_html_change_produces_text_diff() {
        let l = et("html", "line a\nline b\nline c\n");
        let mut r = et("html", "line a\nold b\nline c\n");
        r.body_html = "line a\nold b\nline c\n".into();
        // sync the plaintext so only html differs
        r.body_plaintext = l.body_plaintext.clone();
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        let td = d.body_html_diff.expect("html diff present");
        assert_eq!(td.additions, 1);
        assert_eq!(td.deletions, 1);
        assert!(d.body_plaintext_diff.is_none());
    }

    #[test]
    fn body_plaintext_change_produces_text_diff() {
        let l = et("txt", "<p>same</p>");
        let mut r = et("txt", "<p>same</p>");
        r.body_plaintext = "different plain".into();
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        assert!(d.body_html_diff.is_none());
        assert!(d.body_plaintext_diff.is_some());
    }

    #[test]
    fn description_difference_alone_is_not_drift() {
        let mut l = et("desc", "<p>body</p>");
        let r = et("desc", "<p>body</p>");
        l.description = Some("new desc".into());
        // Make sure description is the ONLY difference
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged), "got {:?}", d.op);
        assert!(!d.has_changes());
    }

    #[test]
    fn tag_reorder_alone_is_not_drift() {
        let mut l = et("tags", "<p>body</p>");
        let mut r = et("tags", "<p>body</p>");
        l.tags = vec!["alpha".into(), "beta".into(), "gamma".into()];
        r.tags = vec!["gamma".into(), "alpha".into(), "beta".into()];
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged), "got {:?}", d.op);
        assert!(!d.has_changes());
    }

    #[test]
    fn tag_change_is_modified_with_metadata_flag() {
        let mut l = et("tags2", "<p>body</p>");
        let r = et("tags2", "<p>body</p>");
        l.tags = vec!["a".into(), "b".into()];
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        assert!(d.metadata_changed);
        assert!(!d.subject_changed);
    }

    #[test]
    fn preheader_none_equals_empty() {
        let mut l = et("pre", "<p>body</p>");
        let mut r = et("pre", "<p>body</p>");
        l.preheader = Some(String::new());
        r.preheader = None;
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Unchanged), "got {:?}", d.op);
        assert!(!d.has_changes());
    }

    #[test]
    fn should_inline_css_change_is_metadata_modified() {
        let mut l = et("css", "<p>body</p>");
        let r = et("css", "<p>body</p>");
        l.should_inline_css = Some(false);
        let d = diff(Some(&l), Some(&r)).unwrap();
        assert!(matches!(d.op, DiffOp::Modified { .. }));
        assert!(d.metadata_changed);
    }

    #[test]
    fn destructive_count_is_never_set_on_email_templates() {
        let r = et("ghost", "<p>x</p>");
        let orphan = diff(None, Some(&r)).unwrap();
        assert!(!orphan.op.is_destructive());

        let l2 = et("changed", "<p>new</p>");
        let r2 = et("changed", "<p>old</p>");
        let modified = diff(Some(&l2), Some(&r2)).unwrap();
        assert!(!modified.op.is_destructive());
    }
}
