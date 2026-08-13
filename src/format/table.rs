//! Human-readable table formatter for diff results.
//!
//! Renders a [`crate::diff::DiffSummary`] as an indented multi-resource
//! layout. v0.1.0 ships without ANSI colors; `--no-color` is a no-op
//! until a future cosmetic pass.

use crate::diff::catalog::CatalogSchemaDiff;
use crate::diff::content_block::ContentBlockDiff;
use crate::diff::custom_attribute::{CustomAttributeDiff, CustomAttributeOp};
use crate::diff::email_template::EmailTemplateDiff;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::resource::{CatalogField, ResourceKind};
use std::fmt::Write as _;

/// The entire body of an in-sync resource that carries no extra
/// information. `only_drift` suppresses exactly the blocks whose body
/// equals this — an unchanged resource that still rendered an
/// informational line (e.g. a Custom Attribute type mismatch) has a
/// longer body and stays visible.
const NO_DRIFT_BODY: &str = "   no drift\n";

pub fn render(summary: &DiffSummary, only_drift: bool) -> String {
    let mut out = String::new();

    for diff in &summary.diffs {
        let body = render_body(diff);
        if only_drift && body == NO_DRIFT_BODY {
            continue;
        }
        render_header(&mut out, diff);
        out.push_str(&body);
        out.push('\n');
    }

    let orphans = summary.orphan_count();
    let _ = writeln!(
        out,
        "Summary: {} changed, {} in sync, {orphans} orphan, {} destructive",
        summary.changed_count(),
        summary.in_sync_count(),
        summary.destructive_count(),
    );

    // Always-on orphan report. Braze exposes no DELETE for content blocks
    // or email templates, so braze-sync cannot prune them; surface them as
    // a read-only signal instead of mutating remote state.
    if orphans > 0 {
        let _ = writeln!(
            out,
            "\nℹ {orphans} Braze resource(s) not present in Git. \
             Archive them in the Braze dashboard if intended, \
             or add them to exclude_patterns to keep them.",
        );
    }

    out
}

fn render_header(out: &mut String, diff: &ResourceDiff) {
    let icon = if diff.has_changes() {
        kind_icon(diff.kind())
    } else {
        "✅"
    };
    let label = kind_label(diff.kind());
    let _ = writeln!(out, "{icon} {label}: {}", diff.name());
}

/// Render the indented lines under a resource header. Built separately
/// from the header so `only_drift` can decide whether the block is worth
/// printing at all by inspecting what the body actually says.
fn render_body(diff: &ResourceDiff) -> String {
    let mut out = String::new();

    if !diff.has_changes() {
        out.push_str(NO_DRIFT_BODY);
        // Custom Attributes may carry informational hints (e.g. type
        // mismatch) even when unchanged.
        if let ResourceDiff::CustomAttribute(d) = diff {
            render_custom_attribute(&mut out, d);
        }
        return out;
    }

    match diff {
        ResourceDiff::CatalogSchema(d) => render_catalog_schema(&mut out, d),
        ResourceDiff::ContentBlock(d) => render_content_block(&mut out, d),
        ResourceDiff::EmailTemplate(d) => render_email_template(&mut out, d),
        ResourceDiff::CustomAttribute(d) => render_custom_attribute(&mut out, d),
        ResourceDiff::Tag(d) => render_tag(&mut out, d),
    }
    out
}

fn kind_icon(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::CatalogSchema => "📋",
        ResourceKind::ContentBlock => "📝",
        ResourceKind::EmailTemplate => "📧",
        ResourceKind::CustomAttribute => "🏷 ",
        ResourceKind::Tag => "🔖",
    }
}

fn kind_label(kind: ResourceKind) -> &'static str {
    match kind {
        ResourceKind::CatalogSchema => "Catalog Schema",
        ResourceKind::ContentBlock => "Content Block",
        ResourceKind::EmailTemplate => "Email Template",
        ResourceKind::CustomAttribute => "Custom Attribute",
        ResourceKind::Tag => "Tag",
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

fn render_tag(out: &mut String, d: &crate::diff::tag::TagDiff) {
    use crate::diff::tag::TagOp;
    match &d.op {
        TagOp::ReferencedButUnregistered => {
            out.push_str(
                "   ⚠ referenced by a resource but not in tags/registry.yaml \
                 (apply will fail until added + created in Braze dashboard)\n",
            );
        }
        TagOp::RegisteredButUnreferenced => {
            out.push_str("   ℹ in tags/registry.yaml but no local resource references it\n");
        }
        TagOp::Unchanged => {}
    }
    for hint in &d.hints {
        let _ = writeln!(out, "   ℹ {hint}");
    }
}
