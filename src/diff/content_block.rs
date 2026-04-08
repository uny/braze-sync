//! Content Block diff — Phase A2 stub.
//!
//! Full implementation (text diff via `similar`, orphan tracking, etc.)
//! lands in Phase B1. The struct is defined here so
//! [`crate::diff::ResourceDiff`] compiles end-to-end during Phase A.

use crate::diff::DiffOp;
use crate::resource::ContentBlock;

#[derive(Debug, Clone)]
pub struct ContentBlockDiff {
    pub name: String,
    pub op: DiffOp<ContentBlock>,
    pub text_diff: Option<TextDiffSummary>,
    /// Braze にあるが Git にない場合 true。§11.6 参照。
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
