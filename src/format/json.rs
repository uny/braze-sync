//! JSON formatter for diff results.
//!
//! The JSON shape here is **frozen at v1.0** with an explicit
//! `version: 1` field on the root. New fields may be added (additive
//! only); existing fields cannot be renamed or removed without bumping
//! `version`. CI consumers can branch on `version` to support a future
//! v2 schema. See IMPLEMENTATION.md §12 / §2.5.
//!
//! Wire types are deliberately separate from `crate::diff` /
//! `crate::resource` types so refactoring a domain type cannot
//! accidentally change the public JSON contract. Conversion happens at
//! the boundary in [`From`] impls.

use crate::diff::catalog::{CatalogItemsDiff, CatalogSchemaDiff};
use crate::diff::content_block::ContentBlockDiff;
use crate::diff::custom_attribute::{CustomAttributeDiff, CustomAttributeOp};
use crate::diff::email_template::EmailTemplateDiff;
use crate::diff::TextDiffSummary;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::resource::CatalogField;
use serde::Serialize;

pub fn render(summary: &DiffSummary) -> String {
    let root = JsonRoot::from(summary);
    let mut s = serde_json::to_string_pretty(&root).expect("internal wire types serialize cleanly");
    // Formatter contract: render returns a display-ready string ending
    // with exactly one newline. table::render already does; this matches.
    // insta strips trailing newlines, so the existing snapshots are
    // unaffected.
    s.push('\n');
    s
}

// =====================================================================
// Wire types — public JSON contract, frozen at v1.0.
// =====================================================================

#[derive(Serialize)]
struct JsonRoot {
    version: u32,
    summary: JsonSummary,
    diffs: Vec<JsonDiffEntry>,
}

#[derive(Serialize)]
struct JsonSummary {
    changed: usize,
    in_sync: usize,
    destructive: usize,
    orphan: usize,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JsonDiffEntry {
    CatalogSchema {
        name: String,
        op: JsonOp,
        field_diffs: Vec<JsonFieldDiff>,
    },
    CatalogItems {
        catalog_name: String,
        added: usize,
        modified: usize,
        removed: usize,
        unchanged: usize,
    },
    ContentBlock {
        name: String,
        op: JsonOp,
        orphan: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        text_diff: Option<JsonTextDiff>,
    },
    EmailTemplate {
        name: String,
        op: JsonOp,
        subject_changed: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        body_html_diff: Option<JsonTextDiff>,
        #[serde(skip_serializing_if = "Option::is_none")]
        body_plaintext_diff: Option<JsonTextDiff>,
        metadata_changed: bool,
        orphan: bool,
    },
    CustomAttribute {
        name: String,
        #[serde(flatten)]
        change: JsonCustomAttributeChange,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        hints: Vec<String>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum JsonOp {
    Added,
    Removed,
    Modified,
    Unchanged,
}

#[derive(Serialize)]
struct JsonField {
    name: String,
    #[serde(rename = "type")]
    field_type: &'static str,
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum JsonFieldDiff {
    Added { field: JsonField },
    Removed { field: JsonField },
    Modified { from: JsonField, to: JsonField },
}

#[derive(Serialize)]
struct JsonTextDiff {
    additions: usize,
    deletions: usize,
}

#[derive(Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum JsonCustomAttributeChange {
    DeprecationToggled { from: bool, to: bool },
    UnregisteredInGit,
    PresentInGitOnly,
    MetadataOnly,
    Unchanged,
}

// =====================================================================
// Domain → Wire conversion at the boundary.
// =====================================================================

impl From<&DiffSummary> for JsonRoot {
    fn from(s: &DiffSummary) -> Self {
        Self {
            version: 1,
            summary: JsonSummary {
                changed: s.changed_count(),
                in_sync: s.in_sync_count(),
                destructive: s.destructive_count(),
                orphan: s.orphan_count(),
            },
            diffs: s.diffs.iter().map(JsonDiffEntry::from).collect(),
        }
    }
}

impl From<&ResourceDiff> for JsonDiffEntry {
    fn from(d: &ResourceDiff) -> Self {
        match d {
            ResourceDiff::CatalogSchema(c) => Self::from_catalog_schema(c),
            ResourceDiff::CatalogItems(c) => Self::from_catalog_items(c),
            ResourceDiff::ContentBlock(c) => Self::from_content_block(c),
            ResourceDiff::EmailTemplate(c) => Self::from_email_template(c),
            ResourceDiff::CustomAttribute(c) => Self::from_custom_attribute(c),
        }
    }
}

impl JsonDiffEntry {
    fn from_catalog_schema(c: &CatalogSchemaDiff) -> Self {
        Self::CatalogSchema {
            name: c.name.clone(),
            op: top_op(&c.op),
            field_diffs: c.field_diffs.iter().filter_map(json_field_diff).collect(),
        }
    }

    fn from_catalog_items(c: &CatalogItemsDiff) -> Self {
        Self::CatalogItems {
            catalog_name: c.catalog_name.clone(),
            added: c.added_ids.len(),
            modified: c.modified_ids.len(),
            removed: c.removed_ids.len(),
            unchanged: c.unchanged_count,
        }
    }

    fn from_content_block(c: &ContentBlockDiff) -> Self {
        Self::ContentBlock {
            name: c.name.clone(),
            op: top_op(&c.op),
            orphan: c.orphan,
            text_diff: c.text_diff.as_ref().map(json_text_diff),
        }
    }

    fn from_email_template(c: &EmailTemplateDiff) -> Self {
        Self::EmailTemplate {
            name: c.name.clone(),
            op: top_op(&c.op),
            subject_changed: c.subject_changed,
            body_html_diff: c.body_html_diff.as_ref().map(json_text_diff),
            body_plaintext_diff: c.body_plaintext_diff.as_ref().map(json_text_diff),
            metadata_changed: c.metadata_changed,
            orphan: c.orphan,
        }
    }

    fn from_custom_attribute(c: &CustomAttributeDiff) -> Self {
        Self::CustomAttribute {
            name: c.name.clone(),
            change: json_custom_attribute_change(&c.op),
            hints: c.hints.clone(),
        }
    }
}

fn top_op<T>(op: &DiffOp<T>) -> JsonOp {
    match op {
        DiffOp::Added(_) => JsonOp::Added,
        DiffOp::Removed(_) => JsonOp::Removed,
        DiffOp::Modified { .. } => JsonOp::Modified,
        DiffOp::Unchanged => JsonOp::Unchanged,
    }
}

fn json_field(f: &CatalogField) -> JsonField {
    JsonField {
        name: f.name.clone(),
        field_type: f.field_type.as_str(),
    }
}

fn json_field_diff(d: &DiffOp<CatalogField>) -> Option<JsonFieldDiff> {
    Some(match d {
        DiffOp::Added(f) => JsonFieldDiff::Added {
            field: json_field(f),
        },
        DiffOp::Removed(f) => JsonFieldDiff::Removed {
            field: json_field(f),
        },
        DiffOp::Modified { from, to } => JsonFieldDiff::Modified {
            from: json_field(from),
            to: json_field(to),
        },
        DiffOp::Unchanged => return None,
    })
}

fn json_text_diff(t: &TextDiffSummary) -> JsonTextDiff {
    JsonTextDiff {
        additions: t.additions,
        deletions: t.deletions,
    }
}

fn json_custom_attribute_change(op: &CustomAttributeOp) -> JsonCustomAttributeChange {
    match op {
        CustomAttributeOp::DeprecationToggled { from, to } => {
            JsonCustomAttributeChange::DeprecationToggled {
                from: *from,
                to: *to,
            }
        }
        CustomAttributeOp::UnregisteredInGit => JsonCustomAttributeChange::UnregisteredInGit,
        CustomAttributeOp::PresentInGitOnly => JsonCustomAttributeChange::PresentInGitOnly,
        CustomAttributeOp::MetadataOnly => JsonCustomAttributeChange::MetadataOnly,
        CustomAttributeOp::Unchanged => JsonCustomAttributeChange::Unchanged,
    }
}

#[cfg(test)]
mod parses_back_tests {
    //! Sanity that whatever we emit is at least valid JSON. Snapshot
    //! tests in `snapshot_tests.rs` lock the exact text.
    use super::render;
    use crate::diff::DiffSummary;

    #[test]
    fn empty_summary_renders_valid_json_with_version_1() {
        let s = render(&DiffSummary::default());
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["version"], serde_json::json!(1));
        assert!(v["diffs"].as_array().unwrap().is_empty());
    }
}
