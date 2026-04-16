//! Human-readable table formatter for diff results.
//!
//! Renders a [`crate::diff::DiffSummary`] as the indented multi-resource
//! layout shown in IMPLEMENTATION.md §7.4. v0.1.0 ships without ANSI
//! colors; `--no-color` is a no-op until a future cosmetic pass.

use crate::diff::catalog::{CatalogItemsDiff, CatalogSchemaDiff};
use crate::diff::content_block::ContentBlockDiff;
use crate::diff::custom_attribute::{CustomAttributeDiff, CustomAttributeOp};
use crate::diff::email_template::EmailTemplateDiff;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::resource::{CatalogField, ResourceKind};
use std::fmt::Write as _;

pub fn render(summary: &DiffSummary) -> String {
    let mut out = String::new();

    for diff in &summary.diffs {
        render_one(&mut out, diff);
        out.push('\n');
    }

    let _ = writeln!(
        out,
        "Summary: {} changed, {} in sync, {} orphan, {} destructive",
        summary.changed_count(),
        summary.in_sync_count(),
        summary.orphan_count(),
        summary.destructive_count(),
    );

    out
}

fn render_one(out: &mut String, diff: &ResourceDiff) {
    let unchanged = !diff.has_changes();
    let icon = if unchanged {
        "✅"
    } else {
        kind_icon(diff.kind())
    };
    let label = kind_label(diff.kind());
    let _ = writeln!(out, "{icon} {label}: {}", diff.name());

    if unchanged {
        out.push_str("   no drift\n");
        // Custom Attributes may carry informational hints (e.g. type
        // mismatch) even when unchanged.
        if let ResourceDiff::CustomAttribute(d) = diff {
            render_custom_attribute(out, d);
        }
        return;
    }

    match diff {
        ResourceDiff::CatalogSchema(d) => render_catalog_schema(out, d),
        ResourceDiff::CatalogItems(d) => render_catalog_items(out, d),
        ResourceDiff::ContentBlock(d) => render_content_block(out, d),
        ResourceDiff::EmailTemplate(d) => render_email_template(out, d),
        ResourceDiff::CustomAttribute(d) => render_custom_attribute(out, d),
    }
}

fn kind_icon(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::CatalogSchema => "📋",
        ResourceKind::CatalogItems => "📦",
        ResourceKind::ContentBlock => "📝",
        ResourceKind::EmailTemplate => "📧",
        ResourceKind::CustomAttribute => "🏷 ",
    }
}

fn kind_label(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::CatalogSchema => "Catalog Schema",
        ResourceKind::CatalogItems => "Catalog Items",
        ResourceKind::ContentBlock => "Content Block",
        ResourceKind::EmailTemplate => "Email Template",
        ResourceKind::CustomAttribute => "Custom Attribute",
    }
}

fn fmt_field(f: &CatalogField) -> String {
    format!("{} ({})", f.name, f.field_type.as_str())
}

fn render_catalog_schema(out: &mut String, d: &CatalogSchemaDiff) {
    if matches!(d.op, DiffOp::Added(_)) {
        out.push_str("   + new catalog\n");
    } else if matches!(d.op, DiffOp::Removed(_)) {
        out.push_str("   - removed catalog (destructive)\n");
    }
    for fd in &d.field_diffs {
        match fd {
            DiffOp::Added(f) => {
                let _ = writeln!(out, "   + field: {}", fmt_field(f));
            }
            DiffOp::Removed(f) => {
                let _ = writeln!(out, "   - field: {}", fmt_field(f));
            }
            DiffOp::Modified { from, to } => {
                let _ = writeln!(
                    out,
                    "   ~ field: {} ({} → {})",
                    to.name,
                    from.field_type.as_str(),
                    to.field_type.as_str(),
                );
            }
            DiffOp::Unchanged => {}
        }
    }
}

fn render_catalog_items(out: &mut String, d: &CatalogItemsDiff) {
    let total = d.added_ids.len() + d.modified_ids.len() + d.removed_ids.len() + d.unchanged_count;
    let _ = writeln!(
        out,
        "   + {} added, ~ {} modified, - {} removed (in {} total)",
        d.added_ids.len(),
        d.modified_ids.len(),
        d.removed_ids.len(),
        total,
    );
}

fn render_content_block(out: &mut String, d: &ContentBlockDiff) {
    if d.orphan {
        out.push_str("   ⚠ orphaned (exists in Braze, not in Git)\n");
        return;
    }
    match &d.op {
        DiffOp::Added(_) => out.push_str("   + new content block\n"),
        DiffOp::Removed(_) => out.push_str("   - removed content block\n"),
        DiffOp::Modified { .. } => {
            if let Some(td) = &d.text_diff {
                let _ = writeln!(
                    out,
                    "   ~ content changed (+{} -{})",
                    td.additions, td.deletions,
                );
            } else {
                out.push_str("   ~ content changed\n");
            }
        }
        DiffOp::Unchanged => {}
    }
}

fn render_email_template(out: &mut String, d: &EmailTemplateDiff) {
    if d.orphan {
        out.push_str("   ⚠ orphaned (exists in Braze, not in Git)\n");
        return;
    }
    if matches!(d.op, DiffOp::Added(_)) {
        out.push_str("   + new email template\n");
    } else if matches!(d.op, DiffOp::Removed(_)) {
        out.push_str("   - removed email template\n");
    }
    if d.subject_changed {
        out.push_str("   ~ subject changed\n");
    }
    if let Some(td) = &d.body_html_diff {
        let _ = writeln!(
            out,
            "   ~ body_html changed (+{} -{})",
            td.additions, td.deletions
        );
    }
    if let Some(td) = &d.body_plaintext_diff {
        let _ = writeln!(
            out,
            "   ~ body_plaintext changed (+{} -{})",
            td.additions, td.deletions
        );
    }
    if d.metadata_changed {
        out.push_str("   ~ metadata changed\n");
    }
}

fn render_custom_attribute(out: &mut String, d: &CustomAttributeDiff) {
    match &d.op {
        CustomAttributeOp::DeprecationToggled { from, to } => {
            let _ = writeln!(out, "   ~ deprecated: {from} → {to}");
        }
        CustomAttributeOp::UnregisteredInGit => {
            out.push_str("   ⚠ exists in Braze but not in Git registry (run export)\n");
        }
        CustomAttributeOp::PresentInGitOnly => {
            out.push_str("   ⚠ in Git registry but not in Braze (likely a typo)\n");
        }
        CustomAttributeOp::MetadataOnly => {
            out.push_str("   ~ metadata-only change (no API to apply)\n");
        }
        CustomAttributeOp::Unchanged => {}
    }
    for hint in &d.hints {
        let _ = writeln!(out, "   ℹ {hint}");
    }
}
