//! Per-export-run values updates (RFC §2.5 "既存リソース" path).
//!
//! Inputs:
//! - the LOCAL template body, which carries the source of truth for
//!   placeholder positions and (via existing values entries) URL /
//!   `${NAME}` correlation anchors;
//! - the REMOTE body just fetched from Braze, which carries the
//!   fresh lid / cb_id values to write back.
//!
//! Output: in-place updates to a [`ValuesFile`], plus a structured
//! report of how many entries were touched and any warnings the
//! operator should see.

use std::collections::BTreeSet;

use crate::resource::{ContentBlock, EmailTemplate};
use crate::values::correlation::{
    extract_cb_id_values, extract_html_lid_values, extract_plaintext_lid_values, normalize_url,
    LidCorrelation,
};
use crate::values::placeholder::{extract_placeholders, PlaceholderType};
use crate::values::schema::{ContentBlockValues, EmailTemplateValues, FieldValues, ValuesFile};

/// Per-resource summary of an export run.
#[derive(Debug, Default, Clone)]
pub struct ExportUpdates {
    pub lid_updates: usize,
    pub cb_id_updates: usize,
    /// values entries whose `__BRAZESYNC.*__` placeholder is no longer
    /// present in the local template (RFC §2.5 step 6).
    pub orphan_warnings: Vec<String>,
    /// values entries whose URL anchor could not be matched in the
    /// remote body (URL deleted in dashboard, multiple matches, etc).
    pub ambiguity_warnings: Vec<String>,
}

impl ExportUpdates {
    pub fn merge(&mut self, other: ExportUpdates) {
        self.lid_updates += other.lid_updates;
        self.cb_id_updates += other.cb_id_updates;
        self.orphan_warnings.extend(other.orphan_warnings);
        self.ambiguity_warnings.extend(other.ambiguity_warnings);
    }
}

/// Refresh `values.content_block.<local.name>` entries from `remote`,
/// using `local` to determine placeholder positions and orphan status.
///
/// Returns the update summary. Does nothing (returns an empty summary)
/// if `local` has no placeholders — that path keeps the existing
/// verbatim-export behavior.
pub fn refresh_content_block_values(
    local: &ContentBlock,
    remote: &ContentBlock,
    values: &mut ValuesFile,
) -> ExportUpdates {
    let mut report = ExportUpdates::default();
    let referenced = referenced_keys(&local.content);
    if referenced.is_empty() {
        return report;
    }

    let cb_entry = values
        .content_block
        .entry(local.name.clone())
        .or_default();

    let html_pairs = extract_html_lid_values(&remote.content);
    refresh_lid_entries(
        &mut cb_entry.lid,
        &html_pairs,
        &format!("content_block '{}' lid", local.name),
        &mut report,
    );

    let cb_id_pairs = extract_cb_id_values(&remote.content);
    refresh_cb_id_entries(
        &mut cb_entry.cb_id,
        &cb_id_pairs,
        &format!("content_block '{}' cb_id", local.name),
        &mut report,
    );

    flag_orphans(cb_entry, &referenced, &local.name, &mut report);
    report
}

/// Refresh `values.email_template.<local.name>` entries field by field.
pub fn refresh_email_template_values(
    local: &EmailTemplate,
    remote: &EmailTemplate,
    values: &mut ValuesFile,
) -> ExportUpdates {
    let mut report = ExportUpdates::default();

    let subject_refs = referenced_keys(&local.subject);
    let body_html_refs = referenced_keys(&local.body_html);
    let body_plain_refs = referenced_keys(&local.body_plaintext);
    let preheader_refs = referenced_keys(local.preheader.as_deref().unwrap_or(""));

    let any_refs = !(subject_refs.is_empty()
        && body_html_refs.is_empty()
        && body_plain_refs.is_empty()
        && preheader_refs.is_empty());
    if !any_refs {
        return report;
    }

    let et_entry = values
        .email_template
        .entry(local.name.clone())
        .or_default();

    refresh_field(
        &mut et_entry.body_html,
        &extract_html_lid_values(&remote.body_html),
        &extract_cb_id_values(&remote.body_html),
        &local.name,
        "body_html",
        &mut report,
    );
    refresh_field(
        &mut et_entry.body_plaintext,
        &extract_plaintext_lid_values(&remote.body_plaintext),
        &extract_cb_id_values(&remote.body_plaintext),
        &local.name,
        "body_plaintext",
        &mut report,
    );
    // subject / preheader: anchor-based lid match isn't covered in this
    // first cut; we still refresh cb_id (rare in these fields) so users
    // who template a {{content_blocks.${…}}} include get correct values.
    refresh_field(
        &mut et_entry.subject,
        &[],
        &extract_cb_id_values(&remote.subject),
        &local.name,
        "subject",
        &mut report,
    );
    if let Some(preheader) = remote.preheader.as_deref() {
        refresh_field(
            &mut et_entry.preheader,
            &[],
            &extract_cb_id_values(preheader),
            &local.name,
            "preheader",
            &mut report,
        );
    }

    flag_email_template_orphans(
        et_entry,
        &subject_refs,
        &preheader_refs,
        &body_html_refs,
        &body_plain_refs,
        &local.name,
        &mut report,
    );
    report
}

fn referenced_keys(body: &str) -> ReferencedKeys {
    let mut out = ReferencedKeys::default();
    for ph in extract_placeholders(body) {
        match ph.ty {
            PlaceholderType::Lid => {
                out.lid.insert(ph.key);
            }
            PlaceholderType::CbId => {
                out.cb_id.insert(ph.key);
            }
            PlaceholderType::Custom | PlaceholderType::Global => {}
        }
    }
    out
}

#[derive(Debug, Default)]
struct ReferencedKeys {
    lid: BTreeSet<String>,
    cb_id: BTreeSet<String>,
}

impl ReferencedKeys {
    fn is_empty(&self) -> bool {
        self.lid.is_empty() && self.cb_id.is_empty()
    }
}

fn refresh_lid_entries(
    entries: &mut std::collections::BTreeMap<String, crate::values::schema::LidEntry>,
    remote_pairs: &[LidCorrelation],
    scope_label: &str,
    report: &mut ExportUpdates,
) {
    for (key, entry) in entries.iter_mut() {
        let Some(url) = entry.url.clone() else {
            continue;
        };
        let needle = normalize_url(&url);
        let matches: Vec<&LidCorrelation> = remote_pairs.iter().filter(|p| p.url == needle).collect();
        match matches.len() {
            0 => {
                report.ambiguity_warnings.push(format!(
                    "{scope_label}.{key}: url '{needle}' not found in remote body — keeping existing value"
                ));
            }
            1 => {
                let new_value = matches[0].value.clone();
                if entry.value.as_deref() != Some(new_value.as_str()) {
                    entry.value = Some(new_value);
                    report.lid_updates += 1;
                }
            }
            _ => {
                report.ambiguity_warnings.push(format!(
                    "{scope_label}.{key}: url '{needle}' matched {} times in remote body — applied positional (first); review",
                    matches.len()
                ));
                let new_value = matches[0].value.clone();
                if entry.value.as_deref() != Some(new_value.as_str()) {
                    entry.value = Some(new_value);
                    report.lid_updates += 1;
                }
            }
        }
    }
}

fn refresh_cb_id_entries(
    entries: &mut std::collections::BTreeMap<String, crate::values::schema::CbIdEntry>,
    remote_pairs: &[crate::values::correlation::CbIdCorrelation],
    scope_label: &str,
    report: &mut ExportUpdates,
) {
    for (key, entry) in entries.iter_mut() {
        // cb_id key == slug(NAME). Match by key.
        let matches: Vec<&crate::values::correlation::CbIdCorrelation> =
            remote_pairs.iter().filter(|p| p.key == *key).collect();
        match matches.len() {
            0 => {
                report.ambiguity_warnings.push(format!(
                    "{scope_label}.{key}: no `${{{}}} | id:` token found in remote body — keeping existing value",
                    key
                ));
            }
            1 => {
                let new_value = matches[0].value.clone();
                if entry.value.as_deref() != Some(new_value.as_str()) {
                    entry.value = Some(new_value);
                    report.cb_id_updates += 1;
                }
            }
            _ => {
                report.ambiguity_warnings.push(format!(
                    "{scope_label}.{key}: matched {} times in remote body — applied positional (first); review",
                    matches.len()
                ));
                let new_value = matches[0].value.clone();
                if entry.value.as_deref() != Some(new_value.as_str()) {
                    entry.value = Some(new_value);
                    report.cb_id_updates += 1;
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn refresh_field(
    field: &mut FieldValues,
    html_pairs: &[LidCorrelation],
    cb_id_pairs: &[crate::values::correlation::CbIdCorrelation],
    resource: &str,
    field_name: &str,
    report: &mut ExportUpdates,
) {
    refresh_lid_entries(
        &mut field.lid,
        html_pairs,
        &format!("email_template '{}' ({}) lid", resource, field_name),
        report,
    );
    refresh_cb_id_entries(
        &mut field.cb_id,
        cb_id_pairs,
        &format!("email_template '{}' ({}) cb_id", resource, field_name),
        report,
    );
}

fn flag_orphans(
    cb_entry: &ContentBlockValues,
    referenced: &ReferencedKeys,
    name: &str,
    report: &mut ExportUpdates,
) {
    for key in cb_entry.lid.keys() {
        if !referenced.lid.contains(key) {
            report.orphan_warnings.push(format!(
                "content_block '{name}' values has orphan lid key '{key}' \
                 (no placeholder references it). Remove manually if intended."
            ));
        }
    }
    for key in cb_entry.cb_id.keys() {
        if !referenced.cb_id.contains(key) {
            report.orphan_warnings.push(format!(
                "content_block '{name}' values has orphan cb_id key '{key}' \
                 (no placeholder references it). Remove manually if intended."
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flag_email_template_orphans(
    et_entry: &EmailTemplateValues,
    subject_refs: &ReferencedKeys,
    preheader_refs: &ReferencedKeys,
    body_html_refs: &ReferencedKeys,
    body_plain_refs: &ReferencedKeys,
    name: &str,
    report: &mut ExportUpdates,
) {
    for (field_name, field, refs) in [
        ("subject", &et_entry.subject, subject_refs),
        ("preheader", &et_entry.preheader, preheader_refs),
        ("body_html", &et_entry.body_html, body_html_refs),
        ("body_plaintext", &et_entry.body_plaintext, body_plain_refs),
    ] {
        for key in field.lid.keys() {
            if !refs.lid.contains(key) {
                report.orphan_warnings.push(format!(
                    "email_template '{name}' ({field_name}) values has orphan lid key '{key}' \
                     (no placeholder references it). Remove manually if intended."
                ));
            }
        }
        for key in field.cb_id.keys() {
            if !refs.cb_id.contains(key) {
                report.orphan_warnings.push(format!(
                    "email_template '{name}' ({field_name}) values has orphan cb_id key '{key}' \
                     (no placeholder references it). Remove manually if intended."
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::content_block::ContentBlockState;
    use crate::values::schema::{CbIdEntry, LidEntry};

    fn cb(name: &str, body: &str) -> ContentBlock {
        ContentBlock {
            name: name.into(),
            description: None,
            content: body.into(),
            tags: Vec::new(),
            state: ContentBlockState::Active,
        }
    }

    fn et(name: &str) -> EmailTemplate {
        EmailTemplate {
            name: name.into(),
            subject: String::new(),
            body_html: String::new(),
            body_plaintext: String::new(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn refreshes_lid_value_from_remote_via_url_anchor() {
        let local = cb(
            "promo",
            r#"<a href="https://example.com/cta">{{ x | lid: '__BRAZESYNC.lid.cta__' }}go</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://example.com/cta">{{ x | lid: 'newlidvalue1' }}go</a>"#,
        );
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "promo".into(),
            ContentBlockValues {
                lid: [(
                    "cta".to_string(),
                    LidEntry {
                        value: Some("oldlidvalue1".into()),
                        url: Some("https://example.com/cta".into()),
                        anchor: None,
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert_eq!(r.lid_updates, 1);
        assert_eq!(
            values.content_block["promo"].lid["cta"].value.as_deref(),
            Some("newlidvalue1")
        );
    }

    #[test]
    fn returns_no_updates_when_local_has_no_placeholders() {
        let local = cb("plain", "<p>Hello</p>");
        let remote = cb(
            "plain",
            r#"<a href="https://example.com/x">{{ y | lid: 'somelidvalue' }}</a>"#,
        );
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert_eq!(r.lid_updates, 0);
        assert!(!values.content_block.contains_key("plain"));
    }

    #[test]
    fn flags_orphan_keys() {
        let local = cb("promo", "<p>__BRAZESYNC.lid.cta__</p>");
        let remote = cb("promo", "<p>somelidvalue1</p>");
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "promo".into(),
            ContentBlockValues {
                lid: [
                    (
                        "cta".to_string(),
                        LidEntry {
                            value: Some("somelidvalue1".into()),
                            url: Some("https://example.com/cta".into()),
                            anchor: None,
                        },
                    ),
                    (
                        "stale_key".to_string(),
                        LidEntry {
                            value: Some("staaaalee1".into()),
                            url: Some("https://example.com/stale".into()),
                            anchor: None,
                        },
                    ),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert!(r
            .orphan_warnings
            .iter()
            .any(|w| w.contains("stale_key")));
    }

    #[test]
    fn refreshes_cb_id_via_name_slug() {
        let local = cb(
            "page",
            "{{content_blocks.${promo_banner} | id: '__BRAZESYNC.cb_id.promo_banner__'}}",
        );
        let remote = cb(
            "page",
            "{{content_blocks.${promo_banner} | id: 'cb99'}}",
        );
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "page".into(),
            ContentBlockValues {
                cb_id: [(
                    "promo_banner".to_string(),
                    CbIdEntry {
                        value: Some("cb1".into()),
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert_eq!(r.cb_id_updates, 1);
        assert_eq!(
            values.content_block["page"].cb_id["promo_banner"]
                .value
                .as_deref(),
            Some("cb99")
        );
    }

    #[test]
    fn warns_when_url_not_in_remote() {
        let local = cb("promo", "<a>__BRAZESYNC.lid.cta__</a>");
        let remote = cb("promo", "<p>no anchor here</p>");
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "promo".into(),
            ContentBlockValues {
                lid: [(
                    "cta".to_string(),
                    LidEntry {
                        value: Some("oldvalueeeee".into()),
                        url: Some("https://example.com/cta".into()),
                        anchor: None,
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert_eq!(r.lid_updates, 0);
        assert!(r
            .ambiguity_warnings
            .iter()
            .any(|w| w.contains("not found")));
    }

    #[test]
    fn email_template_refreshes_per_field() {
        let mut local = et("welcome");
        local.subject = "__BRAZESYNC.cb_id.shared_block__".into();
        local.body_html = r#"<a href="https://example.com/cta">__BRAZESYNC.lid.cta__</a>"#.into();

        let mut remote = et("welcome");
        remote.subject = "{{content_blocks.${shared_block} | id: 'cb7'}}".into();
        remote.body_html =
            r#"<a href="https://example.com/cta">{{ x | lid: 'newhtmllidx' }}</a>"#.into();

        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.email_template.insert(
            "welcome".into(),
            EmailTemplateValues {
                subject: FieldValues {
                    cb_id: [(
                        "shared_block".to_string(),
                        CbIdEntry {
                            value: Some("cb1".into()),
                        },
                    )]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
                body_html: FieldValues {
                    lid: [(
                        "cta".to_string(),
                        LidEntry {
                            value: Some("oldhtmllidx".into()),
                            url: Some("https://example.com/cta".into()),
                            anchor: None,
                        },
                    )]
                    .into_iter()
                    .collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
        );

        let r = refresh_email_template_values(&local, &remote, &mut values);
        assert_eq!(r.lid_updates, 1);
        assert_eq!(r.cb_id_updates, 1);
        assert_eq!(
            values.email_template["welcome"].body_html.lid["cta"]
                .value
                .as_deref(),
            Some("newhtmllidx")
        );
        assert_eq!(
            values.email_template["welcome"].subject.cb_id["shared_block"]
                .value
                .as_deref(),
            Some("cb7")
        );
    }

    #[test]
    fn merge_combines_reports() {
        let mut a = ExportUpdates {
            lid_updates: 1,
            cb_id_updates: 0,
            orphan_warnings: vec!["o1".into()],
            ambiguity_warnings: vec![],
        };
        let b = ExportUpdates {
            lid_updates: 2,
            cb_id_updates: 1,
            orphan_warnings: vec![],
            ambiguity_warnings: vec!["a1".into()],
        };
        a.merge(b);
        assert_eq!(a.lid_updates, 3);
        assert_eq!(a.cb_id_updates, 1);
        assert_eq!(a.orphan_warnings, vec!["o1".to_string()]);
        assert_eq!(a.ambiguity_warnings, vec!["a1".to_string()]);
    }
}
