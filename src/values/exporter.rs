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

use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
    /// Placeholders referenced in the local template that have no
    /// matching entry in the values file. Symmetric to `orphan_warnings`
    /// (which is the inverse direction). Kept separate so operators can
    /// distinguish "add to values" from "remove from values".
    pub missing_entry_warnings: Vec<String>,
    /// values entries whose URL anchor could not be matched in the
    /// remote body (URL deleted in dashboard, multiple matches, etc).
    pub ambiguity_warnings: Vec<String>,
}

impl ExportUpdates {
    pub fn merge(&mut self, other: ExportUpdates) {
        self.lid_updates += other.lid_updates;
        self.cb_id_updates += other.cb_id_updates;
        self.orphan_warnings.extend(other.orphan_warnings);
        self.missing_entry_warnings
            .extend(other.missing_entry_warnings);
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

    let cb_entry = values.content_block.entry(local.name.clone()).or_default();

    let html_pairs = extract_html_lid_values(&remote.content);
    refresh_lid_entries(
        &mut cb_entry.lid,
        &html_pairs,
        &local.content,
        &referenced.lid,
        &format!("content_block '{}' lid", local.name),
        &mut report,
    );

    let cb_id_pairs = extract_cb_id_values(&remote.content);
    refresh_cb_id_entries(
        &mut cb_entry.cb_id,
        &cb_id_pairs,
        &referenced.cb_id,
        &format!("content_block '{}' cb_id", local.name),
        &mut report,
    );

    flag_orphans(cb_entry, &referenced, &local.name, &mut report);
    flag_missing_entries(
        cb_entry.lid.keys(),
        cb_entry.cb_id.keys(),
        &referenced,
        &format!("content_block '{}'", local.name),
        &mut report,
    );
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

    let et_entry = values.email_template.entry(local.name.clone()).or_default();

    refresh_field(
        &mut et_entry.body_html,
        &extract_html_lid_values(&remote.body_html),
        &extract_cb_id_values(&remote.body_html),
        &local.body_html,
        &body_html_refs,
        &local.name,
        "body_html",
        &mut report,
    );
    refresh_field(
        &mut et_entry.body_plaintext,
        &extract_plaintext_lid_values(&remote.body_plaintext),
        &extract_cb_id_values(&remote.body_plaintext),
        &local.body_plaintext,
        &body_plain_refs,
        &local.name,
        "body_plaintext",
        &mut report,
    );
    // subject / preheader: anchor-based lid match isn't covered in this
    // first cut, so we deliberately skip refresh_lid_entries here —
    // calling it with empty pairs would emit "url not found" warnings
    // for every existing entry. cb_id refresh is still performed so
    // users who template a {{content_blocks.${…}}} include get correct
    // values. preheader is refreshed even when remote.preheader is None
    // so any existing cb_id entry referenced by the local placeholder
    // surfaces a "no token found" warning instead of silently keeping a
    // stale value.
    refresh_cb_id_entries(
        &mut et_entry.subject.cb_id,
        &extract_cb_id_values(&remote.subject),
        &subject_refs.cb_id,
        &format!("email_template '{}' (subject) cb_id", local.name),
        &mut report,
    );
    let preheader_body = remote.preheader.as_deref().unwrap_or("");
    refresh_cb_id_entries(
        &mut et_entry.preheader.cb_id,
        &extract_cb_id_values(preheader_body),
        &preheader_refs.cb_id,
        &format!("email_template '{}' (preheader) cb_id", local.name),
        &mut report,
    );

    flag_email_template_orphans(
        et_entry,
        &subject_refs,
        &preheader_refs,
        &body_html_refs,
        &body_plain_refs,
        &local.name,
        &mut report,
    );
    flag_email_template_missing_entries(
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
    entries: &mut BTreeMap<String, crate::values::schema::LidEntry>,
    remote_pairs: &[LidCorrelation],
    local_body: &str,
    referenced: &BTreeSet<String>,
    scope_label: &str,
    report: &mut ExportUpdates,
) {
    // Group remote matches by normalized URL so multiple local entries
    // sharing one URL each consume a distinct remote occurrence.
    let mut by_url: BTreeMap<String, VecDeque<&LidCorrelation>> = BTreeMap::new();
    let mut remote_count: BTreeMap<String, usize> = BTreeMap::new();
    for p in remote_pairs {
        by_url.entry(p.url.clone()).or_default().push_back(p);
        *remote_count.entry(p.url.clone()).or_default() += 1;
    }
    // Only referenced entries contribute to demand: an orphan sharing a
    // URL with a referenced sibling must not inflate `local_n` into the
    // "positional fallback" warning, nor consume a remote occurrence.
    let mut local_demand: BTreeMap<String, usize> = BTreeMap::new();
    for (key, entry) in entries.iter() {
        if !referenced.contains(key) {
            continue;
        }
        if let Some(url) = &entry.url {
            *local_demand.entry(normalize_url(url)).or_default() += 1;
        }
    }

    // Assignment order = first appearance of each `lid` placeholder
    // in `local_body`. BTreeMap (alphabetical) order would swap values
    // whenever alphabetical order differs from source order. Only
    // referenced keys participate so orphans cannot pop_front the URL
    // bucket and steal a value from a referenced anchor-only sibling.
    let mut order: Vec<String> = Vec::with_capacity(referenced.len());
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for ph in extract_placeholders(local_body) {
        if !matches!(ph.ty, PlaceholderType::Lid) {
            continue;
        }
        if entries.contains_key(&ph.key)
            && referenced.contains(&ph.key)
            && seen.insert(ph.key.clone())
        {
            order.push(ph.key);
        }
    }
    // Referenced keys missing from the placeholder scan still need to
    // be visited so the "url not found" warning fires.
    for key in referenced {
        if entries.contains_key(key) && !seen.contains(key) {
            order.push(key.clone());
        }
    }

    for key in order {
        let entry = entries
            .get_mut(&key)
            .expect("order keys are derived from entries");
        let Some(url) = entry.url.clone() else {
            // Anchor-only correlation is out of scope; warn so the
            // operator knows the stale value persisted.
            report.ambiguity_warnings.push(format!(
                "{scope_label}.{key}: entry has no `url` anchor — anchor-only correlation \
                 is not implemented; keeping existing value"
            ));
            continue;
        };
        let needle = normalize_url(&url);
        let remote_n = *remote_count.get(&needle).unwrap_or(&0);
        let local_n = *local_demand.get(&needle).unwrap_or(&1);
        let pick = by_url
            .get_mut(&needle)
            .and_then(|bucket| bucket.pop_front());
        let Some(pick) = pick else {
            report.ambiguity_warnings.push(format!(
                "{scope_label}.{key}: url '{needle}' not found in remote body \
                 (expected {local_n}, got {remote_n}) — keeping existing value"
            ));
            continue;
        };
        // Note ambiguity only when assignment was non-trivial: either
        // multiple local entries share this URL (positional fallback),
        // or remote has more occurrences than local demands (extras
        // discarded).
        if local_n > 1 || remote_n > local_n {
            report.ambiguity_warnings.push(format!(
                "{scope_label}.{key}: url '{needle}' matched {remote_n} time(s) in remote body \
                 across {local_n} local entry(ies) — applied positional (by local source order); review"
            ));
        }
        if entry.value.as_deref() != Some(pick.value.as_str()) {
            entry.value = Some(pick.value.clone());
            report.lid_updates += 1;
        }
    }
}

fn refresh_cb_id_entries(
    entries: &mut BTreeMap<String, crate::values::schema::CbIdEntry>,
    remote_pairs: &[crate::values::correlation::CbIdCorrelation],
    referenced: &BTreeSet<String>,
    scope_label: &str,
    report: &mut ExportUpdates,
) {
    for (key, entry) in entries.iter_mut() {
        // Orphans must not be rewritten even when a remote ${NAME}
        // happens to slug-match the key — `flag_orphans` owns them.
        if !referenced.contains(key) {
            continue;
        }
        let matches: Vec<&crate::values::correlation::CbIdCorrelation> =
            remote_pairs.iter().filter(|p| p.key == *key).collect();
        match matches.len() {
            0 => {
                report.ambiguity_warnings.push(format!(
                    "{scope_label}.{key}: no `{{{{content_blocks.${{NAME}} | id: …}}}}` \
                     include resolving to key '{key}' found in remote body — \
                     keeping existing value"
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
    local_body: &str,
    refs: &ReferencedKeys,
    resource: &str,
    field_name: &str,
    report: &mut ExportUpdates,
) {
    refresh_lid_entries(
        &mut field.lid,
        html_pairs,
        local_body,
        &refs.lid,
        &format!("email_template '{}' ({}) lid", resource, field_name),
        report,
    );
    refresh_cb_id_entries(
        &mut field.cb_id,
        cb_id_pairs,
        &refs.cb_id,
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

/// Symmetric to [`flag_orphans`]: warn for placeholder references in
/// the local template that have no corresponding values entry. apply /
/// diff pre-flight would fail on these anyway, but surfacing them at
/// export time tells the operator what to add before they run apply.
fn flag_missing_entries<'a>(
    lid_keys: impl Iterator<Item = &'a String>,
    cb_id_keys: impl Iterator<Item = &'a String>,
    referenced: &ReferencedKeys,
    scope_label: &str,
    report: &mut ExportUpdates,
) {
    let lid_present: BTreeSet<&String> = lid_keys.collect();
    for key in &referenced.lid {
        if !lid_present.contains(key) {
            report.missing_entry_warnings.push(format!(
                "{scope_label}: placeholder __BRAZESYNC.lid.{key}__ has no entry in values \
                 (add `lid.{key}: {{ value: …, url: … }}` or run apply pre-flight will fail)"
            ));
        }
    }
    let cb_id_present: BTreeSet<&String> = cb_id_keys.collect();
    for key in &referenced.cb_id {
        if !cb_id_present.contains(key) {
            report.missing_entry_warnings.push(format!(
                "{scope_label}: placeholder __BRAZESYNC.cb_id.{key}__ has no entry in values \
                 (add `cb_id.{key}: {{ value: … }}` or run apply pre-flight will fail)"
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn flag_email_template_missing_entries(
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
        flag_missing_entries(
            field.lid.keys(),
            field.cb_id.keys(),
            refs,
            &format!("email_template '{name}' ({field_name})"),
            report,
        );
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
        assert!(r.orphan_warnings.iter().any(|w| w.contains("stale_key")));
    }

    #[test]
    fn refreshes_cb_id_via_name_slug() {
        let local = cb(
            "page",
            "{{content_blocks.${promo_banner} | id: '__BRAZESYNC.cb_id.promo_banner__'}}",
        );
        let remote = cb("page", "{{content_blocks.${promo_banner} | id: 'cb99'}}");
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
        assert!(r.ambiguity_warnings.iter().any(|w| w.contains("not found")));
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
    fn lid_entry_without_url_anchor_emits_warning_and_is_skipped() {
        // Regression: anchor-only LidEntries (url=None) used to be
        // silently skipped, hiding stale values from the operator.
        let local = cb("promo", "<a>__BRAZESYNC.lid.cta__</a>");
        let remote = cb(
            "promo",
            r#"<a href="https://example.com/cta">{{ x | lid: 'newlidvalue1' }}</a>"#,
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
                        value: Some("oldvalueeeee".into()),
                        url: None,
                        anchor: Some("anchor-only".into()),
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert_eq!(r.lid_updates, 0);
        assert!(
            r.ambiguity_warnings
                .iter()
                .any(|w| w.contains("no `url` anchor")),
            "expected anchor-only warning, got: {:?}",
            r.ambiguity_warnings
        );
    }

    #[test]
    fn distinct_local_entries_sharing_url_get_distinct_remote_values() {
        // Regression: two local placeholder keys with the same anchor
        // URL used to both receive matches[0]; with positional
        // assignment they should each consume a distinct remote
        // occurrence (in BTreeMap key order).
        let local = cb(
            "promo",
            r#"<a href="https://example.com/cta">__BRAZESYNC.lid.cta_a__</a>
               <a href="https://example.com/cta">__BRAZESYNC.lid.cta_b__</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://example.com/cta">{{ x | lid: 'lidaaaaaaa1' }}</a>
               <a href="https://example.com/cta">{{ x | lid: 'lidbbbbbbb2' }}</a>"#,
        );
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "promo".into(),
            ContentBlockValues {
                lid: [
                    (
                        "cta_a".to_string(),
                        LidEntry {
                            value: Some("oldoldoldold".into()),
                            url: Some("https://example.com/cta".into()),
                            anchor: None,
                        },
                    ),
                    (
                        "cta_b".to_string(),
                        LidEntry {
                            value: Some("oldoldoldolb".into()),
                            url: Some("https://example.com/cta".into()),
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
        assert_eq!(r.lid_updates, 2);
        let cta_a = &values.content_block["promo"].lid["cta_a"];
        let cta_b = &values.content_block["promo"].lid["cta_b"];
        assert_eq!(cta_a.value.as_deref(), Some("lidaaaaaaa1"));
        assert_eq!(cta_b.value.as_deref(), Some("lidbbbbbbb2"));
        assert!(
            r.ambiguity_warnings
                .iter()
                .any(|w| w.contains("positional")),
            "expected positional warning, got: {:?}",
            r.ambiguity_warnings
        );
    }

    #[test]
    fn referenced_placeholder_without_values_entry_emits_warning() {
        // Regression: a brand-new __BRAZESYNC.lid.new_cta__ in the
        // local template with no matching values entry used to be
        // silently ignored at export time.
        let local = cb(
            "promo",
            r#"<a href="https://example.com/x">__BRAZESYNC.lid.new_cta__</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://example.com/x">{{ x | lid: 'somelidval1' }}</a>"#,
        );
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert!(
            r.missing_entry_warnings
                .iter()
                .any(|w| w.contains("__BRAZESYNC.lid.new_cta__")),
            "expected missing-entry warning, got: {:?}",
            r.missing_entry_warnings
        );
        // Inverse-direction warnings must NOT leak into the
        // values-without-placeholder bucket.
        assert!(
            r.orphan_warnings.is_empty(),
            "missing-entry warning should not appear in orphan_warnings, got: {:?}",
            r.orphan_warnings
        );
    }

    #[test]
    fn subject_with_existing_lid_entry_does_not_warn_about_missing_url() {
        // Regression: subject's refresh_field used to be called with
        // an empty html_pairs slice, which made refresh_lid_entries
        // emit a spurious "url ... not found in remote body" warning
        // for every existing subject.lid entry. subject lid refresh
        // is intentionally out of scope, so neither update nor warn.
        let mut local = et("welcome");
        local.subject = "{{ x | lid: '__BRAZESYNC.lid.s_lid__' }}Hi".into();

        let mut remote = et("welcome");
        remote.subject = "{{ x | lid: 'somelidnew1' }}Hi".into();

        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.email_template.insert(
            "welcome".into(),
            EmailTemplateValues {
                subject: FieldValues {
                    lid: [(
                        "s_lid".to_string(),
                        LidEntry {
                            value: Some("oldlidvalu1".into()),
                            url: Some("https://example.com/s".into()),
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
        assert!(
            !r.ambiguity_warnings
                .iter()
                .any(|w| w.contains("not found in remote body")),
            "subject lid refresh is unsupported; should not warn 'not found' for it, got: {:?}",
            r.ambiguity_warnings
        );
    }

    #[test]
    fn preheader_none_in_remote_warns_about_missing_cb_id_token() {
        // Regression: when remote.preheader was None the whole
        // refresh_field call was skipped, so an existing preheader
        // cb_id entry was silently left stale with no warning.
        let mut local = et("welcome");
        local.preheader = Some("__BRAZESYNC.cb_id.shared__".into());

        let mut remote = et("welcome");
        remote.preheader = None;

        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.email_template.insert(
            "welcome".into(),
            EmailTemplateValues {
                preheader: FieldValues {
                    cb_id: [(
                        "shared".to_string(),
                        CbIdEntry {
                            value: Some("cb1".into()),
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
        assert!(
            r.ambiguity_warnings
                .iter()
                .any(|w| w.contains("preheader") && w.contains("key 'shared'")),
            "expected preheader missing-token warning, got: {:?}",
            r.ambiguity_warnings
        );
    }

    #[test]
    fn lid_assignment_uses_local_source_order_not_alphabetical_key_order() {
        // Keys whose alphabetical order is the OPPOSITE of their source
        // order, so a key-order assignment would silently swap values.
        let local = cb(
            "promo",
            r#"<a href="https://example.com/cta">__BRAZESYNC.lid.zebra__</a>
               <a href="https://example.com/cta">__BRAZESYNC.lid.apple__</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://example.com/cta">{{ x | lid: 'firstvalu1a' }}</a>
               <a href="https://example.com/cta">{{ x | lid: 'secondval2b' }}</a>"#,
        );
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "promo".into(),
            ContentBlockValues {
                lid: [
                    (
                        "apple".to_string(),
                        LidEntry {
                            value: Some("oldoldoldold".into()),
                            url: Some("https://example.com/cta".into()),
                            anchor: None,
                        },
                    ),
                    (
                        "zebra".to_string(),
                        LidEntry {
                            value: Some("oldoldoldolb".into()),
                            url: Some("https://example.com/cta".into()),
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
        assert_eq!(r.lid_updates, 2);
        let apple = &values.content_block["promo"].lid["apple"];
        let zebra = &values.content_block["promo"].lid["zebra"];
        assert_eq!(zebra.value.as_deref(), Some("firstvalu1a"));
        assert_eq!(apple.value.as_deref(), Some("secondval2b"));
    }

    #[test]
    fn orphan_cb_id_entry_does_not_emit_token_not_found_warning() {
        // Needs at least one placeholder somewhere in local so the
        // refresh runs at all; the cb_id key itself stays orphan.
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "page".into(),
            ContentBlockValues {
                cb_id: [(
                    "stale_block".to_string(),
                    CbIdEntry {
                        value: Some("cb9".into()),
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
        let local = cb(
            "page",
            r#"<a href="https://example.com/x">__BRAZESYNC.lid.cta__</a>"#,
        );
        let remote = cb(
            "page",
            r#"<a href="https://example.com/x">{{ x | lid: 'lidvalueab1' }}</a>"#,
        );
        values.content_block.get_mut("page").unwrap().lid.insert(
            "cta".to_string(),
            LidEntry {
                value: Some("oldlidvalu1".into()),
                url: Some("https://example.com/x".into()),
                anchor: None,
            },
        );
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert!(
            !r.ambiguity_warnings
                .iter()
                .any(|w| w.contains("stale_block") && w.contains("include resolving")),
            "orphan cb_id should not produce a 'token not found' warning, got: {:?}",
            r.ambiguity_warnings
        );
        assert!(
            r.orphan_warnings.iter().any(|w| w.contains("stale_block")),
            "expected orphan warning for stale_block, got: {:?}",
            r.orphan_warnings
        );
    }

    #[test]
    fn orphan_lid_entry_value_is_not_mutated_even_when_url_matches_remote() {
        let local = cb(
            "promo",
            r#"<a href="https://example.com/cta">{{ x | lid: '__BRAZESYNC.lid.cta__' }}go</a>"#,
        );
        let remote = cb(
            "promo",
            r#"<a href="https://example.com/cta">{{ x | lid: 'newlidvalue1' }}go</a>
               <a href="https://example.com/stale">{{ x | lid: 'unwantedval1' }}x</a>"#,
        );
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
                            value: Some("oldlidvalue1".into()),
                            url: Some("https://example.com/cta".into()),
                            anchor: None,
                        },
                    ),
                    (
                        "legacy".to_string(),
                        LidEntry {
                            value: Some("preservedv1".into()),
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
        assert_eq!(r.lid_updates, 1, "only the referenced entry should update");
        assert_eq!(
            values.content_block["promo"].lid["cta"].value.as_deref(),
            Some("newlidvalue1")
        );
        assert_eq!(
            values.content_block["promo"].lid["legacy"].value.as_deref(),
            Some("preservedv1"),
            "orphan lid value must be preserved"
        );
        assert!(
            r.orphan_warnings.iter().any(|w| w.contains("legacy")),
            "expected orphan warning for legacy, got: {:?}",
            r.orphan_warnings
        );
    }

    #[test]
    fn orphan_cb_id_entry_value_is_not_mutated_even_when_remote_includes_name() {
        let local = cb("page", "<p>__BRAZESYNC.lid.cta__</p>");
        let remote = cb(
            "page",
            r#"<a href="https://example.com/x">{{ x | lid: 'lidvalueab1' }}</a>
               {{content_blocks.${stale_block} | id: 'cb42'}}"#,
        );
        let mut values = ValuesFile {
            version: 1,
            ..Default::default()
        };
        values.content_block.insert(
            "page".into(),
            ContentBlockValues {
                lid: [(
                    "cta".to_string(),
                    LidEntry {
                        value: Some("oldlidvalu1".into()),
                        url: Some("https://example.com/x".into()),
                        anchor: None,
                    },
                )]
                .into_iter()
                .collect(),
                cb_id: [(
                    "stale_block".to_string(),
                    CbIdEntry {
                        value: Some("cb9".into()),
                    },
                )]
                .into_iter()
                .collect(),
                ..Default::default()
            },
        );
        let r = refresh_content_block_values(&local, &remote, &mut values);
        assert_eq!(
            r.cb_id_updates, 0,
            "orphan cb_id must not count as an update"
        );
        assert_eq!(
            values.content_block["page"].cb_id["stale_block"]
                .value
                .as_deref(),
            Some("cb9"),
            "orphan cb_id value must be preserved"
        );
        assert!(
            r.orphan_warnings.iter().any(|w| w.contains("stale_block")),
            "expected orphan warning, got: {:?}",
            r.orphan_warnings
        );
    }

    #[test]
    fn duplicate_url_orphan_does_not_trigger_positional_warning() {
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
                lid: [
                    (
                        "cta".to_string(),
                        LidEntry {
                            value: Some("oldlidvalue1".into()),
                            url: Some("https://example.com/cta".into()),
                            anchor: None,
                        },
                    ),
                    (
                        "stale".to_string(),
                        LidEntry {
                            value: Some("staaaalee1".into()),
                            url: Some("https://example.com/cta".into()),
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
        assert!(
            !r.ambiguity_warnings
                .iter()
                .any(|w| w.contains("cta") && w.contains("positional")),
            "no positional warning expected, got: {:?}",
            r.ambiguity_warnings
        );
        assert_eq!(r.lid_updates, 1);
    }

    #[test]
    fn merge_combines_reports() {
        let mut a = ExportUpdates {
            lid_updates: 1,
            cb_id_updates: 0,
            orphan_warnings: vec!["o1".into()],
            missing_entry_warnings: vec!["m1".into()],
            ambiguity_warnings: vec![],
        };
        let b = ExportUpdates {
            lid_updates: 2,
            cb_id_updates: 1,
            orphan_warnings: vec![],
            missing_entry_warnings: vec!["m2".into()],
            ambiguity_warnings: vec!["a1".into()],
        };
        a.merge(b);
        assert_eq!(a.lid_updates, 3);
        assert_eq!(a.cb_id_updates, 1);
        assert_eq!(a.orphan_warnings, vec!["o1".to_string()]);
        assert_eq!(
            a.missing_entry_warnings,
            vec!["m1".to_string(), "m2".to_string()]
        );
        assert_eq!(a.ambiguity_warnings, vec!["a1".to_string()]);
    }
}
