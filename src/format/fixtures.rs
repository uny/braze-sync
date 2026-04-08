//! Shared diff fixtures for the snapshot tests in `snapshot_tests.rs`.
//!
//! Each fixture builds a [`DiffSummary`] in a specific shape so the
//! corresponding TableFormatter / JsonFormatter snapshot is reproducible.

use crate::diff::catalog::{diff_schema, CatalogItemsDiff};
use crate::diff::content_block::{ContentBlockDiff, TextDiffSummary};
use crate::diff::custom_attribute::{CustomAttributeDiff, CustomAttributeOp};
use crate::diff::email_template::EmailTemplateDiff;
use crate::diff::{DiffOp, DiffSummary, ResourceDiff};
use crate::resource::{Catalog, CatalogField, CatalogFieldType, ContentBlock, ContentBlockState};

fn field(name: &str, t: CatalogFieldType) -> CatalogField {
    CatalogField {
        name: name.into(),
        field_type: t,
    }
}

pub fn empty() -> DiffSummary {
    DiffSummary::default()
}

pub fn catalog_schema_with_mixed_field_diffs() -> DiffSummary {
    let local = Catalog {
        name: "cardiology".into(),
        description: Some("Cardiology catalog".into()),
        fields: vec![
            field("id", CatalogFieldType::String),
            field("severity_level", CatalogFieldType::Number),
            field("is_active", CatalogFieldType::Boolean),
        ],
    };
    let remote = Catalog {
        name: "cardiology".into(),
        description: Some("Cardiology catalog".into()),
        fields: vec![
            field("id", CatalogFieldType::String),
            field("legacy_code", CatalogFieldType::String),
            field("is_active", CatalogFieldType::String), // type changed
        ],
    };
    let d = diff_schema(Some(&local), Some(&remote)).unwrap();
    DiffSummary {
        diffs: vec![ResourceDiff::CatalogSchema(d)],
    }
}

pub fn catalog_schema_unchanged() -> DiffSummary {
    let cat = Catalog {
        name: "stable".into(),
        description: None,
        fields: vec![field("id", CatalogFieldType::String)],
    };
    let d = diff_schema(Some(&cat), Some(&cat)).unwrap();
    DiffSummary {
        diffs: vec![ResourceDiff::CatalogSchema(d)],
    }
}

pub fn catalog_items_with_changes() -> DiffSummary {
    let d = CatalogItemsDiff {
        catalog_name: "cardiology".into(),
        added_ids: vec!["af001".into(), "af002".into(), "af003".into()],
        modified_ids: vec!["mod_x".into()],
        removed_ids: vec!["legacy_y".into()],
        unchanged_count: 9842,
    };
    DiffSummary {
        diffs: vec![ResourceDiff::CatalogItems(d)],
    }
}

pub fn content_block_orphan() -> DiffSummary {
    let d = ContentBlockDiff {
        name: "legacy_promo".into(),
        op: DiffOp::Unchanged,
        text_diff: None,
        orphan: true,
    };
    DiffSummary {
        diffs: vec![ResourceDiff::ContentBlock(d)],
    }
}

pub fn all_kinds_mixed() -> DiffSummary {
    // Catalog schema with one added field.
    let cs_local = Catalog {
        name: "cardiology".into(),
        description: None,
        fields: vec![
            field("id", CatalogFieldType::String),
            field("score", CatalogFieldType::Number),
        ],
    };
    let cs_remote = Catalog {
        name: "cardiology".into(),
        description: None,
        fields: vec![field("id", CatalogFieldType::String)],
    };
    let cs = diff_schema(Some(&cs_local), Some(&cs_remote)).unwrap();

    // Catalog items: a small handful of changes.
    let ci = CatalogItemsDiff {
        catalog_name: "cardiology".into(),
        added_ids: vec!["af001".into()],
        modified_ids: vec![],
        removed_ids: vec![],
        unchanged_count: 100,
    };

    // Content block: modified with a text diff.
    let cb_from = ContentBlock {
        name: "promo".into(),
        description: None,
        content: "old".into(),
        tags: vec![],
        state: ContentBlockState::Active,
    };
    let cb_to = ContentBlock {
        content: "new".into(),
        ..cb_from.clone()
    };
    let cb = ContentBlockDiff {
        name: "promo".into(),
        op: DiffOp::Modified {
            from: cb_from,
            to: cb_to,
        },
        text_diff: Some(TextDiffSummary {
            additions: 5,
            deletions: 3,
            unified_hunks: vec![],
        }),
        orphan: false,
    };

    // Email template: subject + body_html changed.
    let et = EmailTemplateDiff {
        name: "welcome".into(),
        op: DiffOp::Unchanged,
        subject_changed: true,
        body_html_diff: Some(TextDiffSummary {
            additions: 20,
            deletions: 8,
            unified_hunks: vec![],
        }),
        body_plaintext_diff: None,
        metadata_changed: false,
        orphan: false,
    };

    // Custom attribute: deprecation flag toggled.
    let ca = CustomAttributeDiff {
        name: "marketing_segment".into(),
        op: CustomAttributeOp::DeprecationToggled {
            from: false,
            to: true,
        },
    };

    DiffSummary {
        diffs: vec![
            ResourceDiff::CatalogSchema(cs),
            ResourceDiff::CatalogItems(ci),
            ResourceDiff::ContentBlock(cb),
            ResourceDiff::EmailTemplate(et),
            ResourceDiff::CustomAttribute(ca),
        ],
    }
}
