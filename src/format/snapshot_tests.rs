//! Snapshot tests for [`super::TableFormatter`] and [`super::JsonFormatter`].
//!
//! Two tests per fixture: one for the table formatter, one for the JSON
//! formatter. Snapshot files live in `src/format/snapshots/`. Run with
//! `INSTA_UPDATE=always cargo test format::snapshot_tests` to refresh.

use super::fixtures;
use super::{DiffFormatter, JsonFormatter, TableFormatter};

// =====================================================================
// empty
// =====================================================================

#[test]
fn empty_table() {
    insta::assert_snapshot!(TableFormatter.format(&fixtures::empty()));
}

#[test]
fn empty_json() {
    insta::assert_snapshot!(JsonFormatter.format(&fixtures::empty()));
}

// =====================================================================
// catalog schema with mixed field diffs (added / removed / modified)
// =====================================================================

#[test]
fn catalog_schema_field_diffs_table() {
    insta::assert_snapshot!(
        TableFormatter.format(&fixtures::catalog_schema_with_mixed_field_diffs())
    );
}

#[test]
fn catalog_schema_field_diffs_json() {
    insta::assert_snapshot!(
        JsonFormatter.format(&fixtures::catalog_schema_with_mixed_field_diffs())
    );
}

// =====================================================================
// catalog schema unchanged (no drift)
// =====================================================================

#[test]
fn catalog_schema_unchanged_table() {
    insta::assert_snapshot!(TableFormatter.format(&fixtures::catalog_schema_unchanged()));
}

#[test]
fn catalog_schema_unchanged_json() {
    insta::assert_snapshot!(JsonFormatter.format(&fixtures::catalog_schema_unchanged()));
}

// =====================================================================
// catalog items
// =====================================================================

#[test]
fn catalog_items_table() {
    insta::assert_snapshot!(TableFormatter.format(&fixtures::catalog_items_with_changes()));
}

#[test]
fn catalog_items_json() {
    insta::assert_snapshot!(JsonFormatter.format(&fixtures::catalog_items_with_changes()));
}

// =====================================================================
// content block added (local-only)
// =====================================================================

#[test]
fn content_block_added_table() {
    insta::assert_snapshot!(TableFormatter.format(&fixtures::content_block_added()));
}

#[test]
fn content_block_added_json() {
    insta::assert_snapshot!(JsonFormatter.format(&fixtures::content_block_added()));
}

// =====================================================================
// content block body modified (text diff present)
// =====================================================================

#[test]
fn content_block_body_modified_table() {
    insta::assert_snapshot!(TableFormatter.format(&fixtures::content_block_body_modified()));
}

#[test]
fn content_block_body_modified_json() {
    insta::assert_snapshot!(JsonFormatter.format(&fixtures::content_block_body_modified()));
}

// =====================================================================
// content block orphan
// =====================================================================

#[test]
fn content_block_orphan_table() {
    insta::assert_snapshot!(TableFormatter.format(&fixtures::content_block_orphan()));
}

#[test]
fn content_block_orphan_json() {
    insta::assert_snapshot!(JsonFormatter.format(&fixtures::content_block_orphan()));
}

// =====================================================================
// all five resource kinds in one summary
// =====================================================================

#[test]
fn all_kinds_mixed_table() {
    insta::assert_snapshot!(TableFormatter.format(&fixtures::all_kinds_mixed()));
}

#[test]
fn all_kinds_mixed_json() {
    insta::assert_snapshot!(JsonFormatter.format(&fixtures::all_kinds_mixed()));
}

// =====================================================================
// custom attribute unchanged with type-mismatch hint
// =====================================================================

#[test]
fn custom_attribute_unchanged_with_hint_table() {
    insta::assert_snapshot!(
        TableFormatter.format(&fixtures::custom_attribute_unchanged_with_hint())
    );
}

#[test]
fn custom_attribute_unchanged_with_hint_json() {
    insta::assert_snapshot!(
        JsonFormatter.format(&fixtures::custom_attribute_unchanged_with_hint())
    );
}
