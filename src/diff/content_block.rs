//! Content Block diff types.

use crate::diff::DiffOp;
use crate::resource::ContentBlock;

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
