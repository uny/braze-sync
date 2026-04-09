//! Email Template diff types.

use crate::diff::content_block::TextDiffSummary;
use crate::diff::DiffOp;

#[derive(Debug, Clone)]
pub struct EmailTemplateDiff {
    pub name: String,
    // TODO(phase-b): change to DiffOp<EmailTemplate> for symmetry with
    // CatalogSchemaDiff — natural to refactor when B2 implements this resource.
    pub op: DiffOp<()>,
    pub subject_changed: bool,
    pub body_html_diff: Option<TextDiffSummary>,
    pub body_plaintext_diff: Option<TextDiffSummary>,
    pub metadata_changed: bool,
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
}
