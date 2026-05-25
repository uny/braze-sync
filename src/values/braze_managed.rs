//! Runtime resolution of Braze-managed placeholders (`lid` / `cb_id`).
//!
//! Resolved at apply/diff time from the remote body via URL / `${NAME}`
//! anchor correlation.
//!
//! New-resource fallback (no remote):
//! - **lid**: placeholder key used as the value; Braze reassigns on
//!   first dashboard open.
//! - **cb_id**: `| id: '…'` filter stripped; Braze derives internally.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex_lite::Regex;

use crate::values::correlation::{
    extract_cb_id_values, extract_html_lid_values, extract_lid_values_unanchored,
    extract_plaintext_lid_values, normalize_url, CbIdCorrelation, LidCorrelation,
};
use crate::values::placeholder::{extract_placeholders, LookupKey, PlaceholderType};
use crate::values::templatize::FieldKind;

/// Per-field result of preparing a templatized body for resolution.
#[derive(Debug, Clone)]
pub struct PreparedTemplate {
    /// Body to feed to `resolve_placeholders` after merging
    /// `additions` into the lookup. May equal the input verbatim, or
    /// differ when cb_id filter stripping was applied (new-resource
    /// path).
    pub body: String,
    /// `(type, key) -> value` entries to merge into the caller's
    /// resolution lookup. Only carries lid / cb_id keys actually
    /// referenced by `body`; never carries custom or global keys.
    pub additions: BTreeMap<LookupKey, String>,
    /// Non-fatal warnings — e.g. ambiguous URL matches, anchor-less
    /// lid placeholders in subject/preheader.
    pub warnings: Vec<String>,
}

/// Prepare a templatized field for resolution.
///
/// `remote` is `None` when the resource does not yet exist in Braze
/// (new resource on first apply). `field` selects the URL-anchor
/// strategy (HTML href, plaintext URL, etc.).
pub fn prepare_field(template: &str, remote: Option<&str>, field: FieldKind) -> PreparedTemplate {
    if !template.contains("__BRAZESYNC.") {
        return PreparedTemplate {
            body: template.to_string(),
            additions: BTreeMap::new(),
            warnings: Vec::new(),
        };
    }

    // For new resources we first strip the cb_id `| id: '…'` filter
    // out of the template entirely. The remaining `__BRAZESYNC.lid.*__`
    // placeholders get fallback values below.
    let (body, mut warnings) = match remote {
        Some(_) => (template.to_string(), Vec::new()),
        None => strip_cb_id_filters(template),
    };

    let mut additions: BTreeMap<LookupKey, String> = BTreeMap::new();

    match remote {
        Some(remote_body) => {
            resolve_lid_from_remote(&body, remote_body, field, &mut additions, &mut warnings);
            resolve_cb_id_from_remote(&body, remote_body, &mut additions, &mut warnings);
        }
        None => {
            fallback_lid_values(&body, &mut additions);
            // cb_id placeholders no longer exist after stripping; nothing
            // to add for cb_id in the new-resource path.
        }
    }

    PreparedTemplate {
        body,
        additions,
        warnings,
    }
}

/// Strip `| id: '__BRAZESYNC.cb_id.<key>__'` filters from a template
/// body. Used for the new-resource fallback path so we POST the
/// documented `{{content_blocks.${NAME}}}` form.
///
/// Returns the rewritten body plus one informational warning per
/// stripped occurrence.
fn strip_cb_id_filters(body: &str) -> (String, Vec<String>) {
    let re = cb_id_filter_re();
    let mut warnings: Vec<String> = Vec::new();
    for cap in re.captures_iter(body) {
        if let Some(key) = cap.get(1) {
            warnings.push(format!(
                "cb_id '{}': new resource — stripping `| id: '…'` filter; \
                 Braze will assign a cb_id on first save",
                key.as_str()
            ));
        }
    }
    let out = re.replace_all(body, "").to_string();
    (out, warnings)
}

fn cb_id_filter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Match `\s*| id: '__BRAZESYNC.cb_id.<key>__'` so the rendered
        // form is `{{content_blocks.${NAME}}}` with no stray pipe.
        Regex::new(r#"\s*\|\s*id:\s*['"]__BRAZESYNC\.cb_id\.([a-z][a-z0-9_]*)__['"]"#)
            .expect("cb_id filter regex is valid")
    })
}

fn fallback_lid_values(body: &str, out: &mut BTreeMap<LookupKey, String>) {
    for ph in extract_placeholders(body) {
        if matches!(ph.ty, PlaceholderType::Lid) {
            out.insert((PlaceholderType::Lid, ph.key.clone()), ph.key);
        }
    }
}

/// Pair lid placeholders in `template` with `| lid: '…'` values in
/// `remote` by URL anchor. Multiple placeholders sharing one anchor
/// URL consume distinct remote occurrences in template appearance
/// order.
fn resolve_lid_from_remote(
    template: &str,
    remote: &str,
    field: FieldKind,
    out: &mut BTreeMap<LookupKey, String>,
    warnings: &mut Vec<String>,
) {
    // subject / preheader: no URL anchors exist. Match remote lid values
    // to template placeholders positionally in field-appearance order.
    if !field.supports_html_anchor() && !field.supports_plaintext_anchor() {
        resolve_lid_positional(template, remote, field, out, warnings);
        return;
    }

    let remote_pairs: Vec<LidCorrelation> = if field.supports_html_anchor() {
        extract_html_lid_values(remote)
    } else {
        extract_plaintext_lid_values(remote)
    };

    // (template_key, optional URL anchor, byte offset) in template
    // appearance order.
    let template_lids = collect_template_lid_anchors(template, field);

    // Group remote pairs by normalized URL, FIFO so positional
    // assignment is deterministic.
    let mut by_url: BTreeMap<String, std::collections::VecDeque<&LidCorrelation>> = BTreeMap::new();
    for p in &remote_pairs {
        by_url.entry(p.url.clone()).or_default().push_back(p);
    }

    // Ambiguity warning: a URL with multiple remote occurrences AND
    // multiple template placeholders means a Dashboard-side link
    // reorder would silently miscorrelate. Emit once per ambiguous URL.
    let mut tmpl_per_url: BTreeMap<String, usize> = BTreeMap::new();
    for (_, anchor, _) in &template_lids {
        if let Some(u) = anchor {
            *tmpl_per_url.entry(u.clone()).or_insert(0) += 1;
        }
    }
    for (url, bucket) in &by_url {
        let tmpl_count = tmpl_per_url.get(url).copied().unwrap_or(0);
        if bucket.len() > 1 && tmpl_count > 1 {
            warnings.push(format!(
                "URL '{url}' has {} remote lid occurrences and {tmpl_count} \
                 template placeholders — using positional FIFO match. \
                 If links were reordered in Braze, lid values may be assigned \
                 to the wrong placeholder.",
                bucket.len()
            ));
        }
    }

    for (key, anchor, _offset) in template_lids {
        let Some(url) = anchor else {
            warnings.push(format!(
                "lid '{key}': placeholder has no URL anchor in template — \
                 anchor-less correlation is not supported; resolve will fail"
            ));
            continue;
        };
        let needle = normalize_url(&url);
        let pick = by_url
            .get_mut(&needle)
            .and_then(|bucket| bucket.pop_front());
        let Some(pick) = pick else {
            // Leave it unresolved — `resolve_placeholders` will surface
            // the unresolved key with full context (offset, etc.).
            warnings.push(format!(
                "lid '{key}': URL anchor '{needle}' not found in remote body"
            ));
            continue;
        };
        out.insert((PlaceholderType::Lid, key), pick.value.clone());
    }
}

/// Positional lid resolution for fields without URL anchors
/// (subject / preheader). Maps the Nth template lid placeholder to the
/// Nth `| lid: '…'` value in the remote field. Counts mismatch is a
/// warning, not a fatal error — leftover unresolved placeholders will
/// still surface via `resolve_placeholders` if they remain unfilled.
fn resolve_lid_positional(
    template: &str,
    remote: &str,
    field: FieldKind,
    out: &mut BTreeMap<LookupKey, String>,
    warnings: &mut Vec<String>,
) {
    let template_keys: Vec<String> = extract_placeholders(template)
        .into_iter()
        .filter(|p| matches!(p.ty, PlaceholderType::Lid))
        .map(|p| p.key)
        .collect();
    if template_keys.is_empty() {
        return;
    }
    let remote_values = extract_lid_values_unanchored(remote);
    let field_label = match field {
        FieldKind::EmailSubject => "subject",
        FieldKind::EmailPreheader => "preheader",
        _ => "field",
    };
    if remote_values.len() != template_keys.len() {
        warnings.push(format!(
            "{field_label} has {} lid placeholder(s) but remote body has {} lid value(s); \
             positional match may misalign — review rendered output",
            template_keys.len(),
            remote_values.len()
        ));
    }
    for (key, value) in template_keys.into_iter().zip(remote_values) {
        out.insert((PlaceholderType::Lid, key), value);
    }
}

/// Pair cb_id placeholders in `template` with `${NAME}` includes in
/// `remote` by Liquid name. Same `${NAME}` referenced twice in the
/// template shares one remote cb_id value.
fn resolve_cb_id_from_remote(
    template: &str,
    remote: &str,
    out: &mut BTreeMap<LookupKey, String>,
    warnings: &mut Vec<String>,
) {
    let remote_pairs = extract_cb_id_values(remote);
    let remote_by_name: BTreeMap<&str, &CbIdCorrelation> =
        remote_pairs.iter().map(|p| (p.name.as_str(), p)).collect();

    for (key, name) in collect_template_cb_id_names(template) {
        match remote_by_name.get(name.as_str()) {
            Some(pick) => {
                out.insert((PlaceholderType::CbId, key), pick.value.clone());
            }
            None => {
                warnings.push(format!(
                    "cb_id '{key}': `${{{name}}}` include not found in remote body"
                ));
            }
        }
    }
}

/// Walk a templatized body and emit `(key, optional URL anchor, offset)`
/// for every `__BRAZESYNC.lid.<key>__`. URL anchor extraction mirrors
/// templatize's `preceding_url` logic but is rerun here because the
/// templatized body has the same surrounding HTML/text as the original.
fn collect_template_lid_anchors(
    body: &str,
    field: FieldKind,
) -> Vec<(String, Option<String>, usize)> {
    let mut out = Vec::new();
    for ph in extract_placeholders(body) {
        if !matches!(ph.ty, PlaceholderType::Lid) {
            continue;
        }
        let anchor = lid_anchor_for(body, ph.start, field);
        out.push((ph.key, anchor, ph.start));
    }
    out
}

/// Walk a templatized body and emit `(key, name)` for every
/// `{{content_blocks.${NAME} | id: '__BRAZESYNC.cb_id.<key>__'}}`.
fn collect_template_cb_id_names(body: &str) -> Vec<(String, String)> {
    let re = cb_id_template_re();
    re.captures_iter(body)
        .filter_map(|cap| {
            let name = cap.get(1)?.as_str().to_string();
            let key = cap.get(2)?.as_str().to_string();
            Some((key, name))
        })
        .collect()
}

fn cb_id_template_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\{\{\s*content_blocks\.\$\{\s*([^\s}|]+)\s*\}\s*\|\s*id:\s*['"]__BRAZESYNC\.cb_id\.([a-z][a-z0-9_]*)__['"]\s*\}\}"#,
        )
        .expect("cb_id template regex is valid")
    })
}

/// Find the URL anchor for a lid placeholder at `offset` in `body`.
///
/// Same precedence as templatize:
/// - HTML / content_block: enclosing element's URL attribute wins;
///   else nearest prior `<a href>`.
/// - Plaintext: nearest prior `https?://…`.
/// - Subject / preheader: no URL anchor (returns None).
fn lid_anchor_for(body: &str, offset: usize, field: FieldKind) -> Option<String> {
    if field.supports_html_anchor() {
        if let Some(tag) = enclosing_open_tag(body, offset) {
            if let Some(url) = url_attr_re()
                .captures(tag)
                .and_then(|c| c.get(1).or(c.get(2)))
            {
                return Some(normalize_url(url.as_str()));
            }
            // Inside a non-URL open tag (e.g. `<a name>` or `<custom data-x>`):
            // do NOT fall through to a prior `<a href>` (matches the
            // templatize semantics that tests pin).
            return None;
        }
        let prefix = &body[..offset];
        anchor_href_re()
            .captures_iter(prefix)
            .last()
            .and_then(|cap| cap.get(1).or(cap.get(2)))
            .map(|m| normalize_url(m.as_str()))
    } else if field.supports_plaintext_anchor() {
        let prefix = &body[..offset];
        plaintext_url_re()
            .find_iter(prefix)
            .last()
            .map(|m| normalize_url(m.as_str()))
    } else {
        None
    }
}

fn enclosing_open_tag(body: &str, offset: usize) -> Option<&str> {
    for m in element_open_tag_re().find_iter(body) {
        if m.start() > offset {
            break;
        }
        if m.end() > offset {
            return Some(&body[m.start()..m.end()]);
        }
    }
    None
}

fn anchor_href_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)<a\b[^>]*?\bhref\s*=\s*(?:"([^"]*)"|'([^']*)')"#)
            .expect("anchor href regex is valid")
    })
}

fn url_attr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)\s(?:[a-z][a-z0-9_-]*:)?(?:href|src|action)\s*=\s*(?:"([^"]*)"|'([^']*)')"#,
        )
        .expect("url attr regex is valid")
    })
}

fn plaintext_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"']+"#).expect("plaintext URL regex is valid"))
}

fn element_open_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)<[a-z][a-z0-9_.:-]*\b[^>]*>"#).expect("element open tag regex is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_placeholders_returns_body_verbatim() {
        let p = prepare_field(
            "<p>hello</p>",
            Some("<p>hello</p>"),
            FieldKind::ContentBlock,
        );
        assert_eq!(p.body, "<p>hello</p>");
        assert!(p.additions.is_empty());
    }

    #[test]
    fn html_lid_resolved_via_url_anchor() {
        let template = r#"<a href="https://example.com/cta">__BRAZESYNC.lid.cta__</a>"#;
        let remote = r#"<a href="https://example.com/cta">{{x | lid: 'newlidvalue1'}}</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert_eq!(
            p.additions
                .get(&(PlaceholderType::Lid, "cta".to_string()))
                .map(String::as_str),
            Some("newlidvalue1")
        );
    }

    #[test]
    fn two_lid_placeholders_sharing_one_url_consume_distinct_remote_values() {
        let template = r#"<a href="https://x.com/a">__BRAZESYNC.lid.a__</a>
<a href="https://x.com/a">__BRAZESYNC.lid.b__</a>"#;
        let remote = r#"<a href="https://x.com/a">{{x | lid: 'firstvalu1a'}}</a>
<a href="https://x.com/a">{{x | lid: 'secondval2b'}}</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert_eq!(
            p.additions[&(PlaceholderType::Lid, "a".into())],
            "firstvalu1a"
        );
        assert_eq!(
            p.additions[&(PlaceholderType::Lid, "b".into())],
            "secondval2b"
        );
    }

    #[test]
    fn cb_id_resolved_via_name() {
        let template =
            "{{content_blocks.${promo_banner} | id: '__BRAZESYNC.cb_id.promo_banner__'}}";
        let remote = "{{content_blocks.${promo_banner} | id: 'cb99'}}";
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert_eq!(
            p.additions
                .get(&(PlaceholderType::CbId, "promo_banner".to_string()))
                .map(String::as_str),
            Some("cb99")
        );
    }

    #[test]
    fn new_resource_lid_falls_back_to_placeholder_key() {
        let template = r#"<a href="https://x.com/cta">__BRAZESYNC.lid.spring_sale__</a>"#;
        let p = prepare_field(template, None, FieldKind::ContentBlock);
        assert_eq!(
            p.additions
                .get(&(PlaceholderType::Lid, "spring_sale".to_string()))
                .map(String::as_str),
            Some("spring_sale")
        );
    }

    #[test]
    fn new_resource_strips_cb_id_filter() {
        let template = "before {{content_blocks.${promo} | id: '__BRAZESYNC.cb_id.promo__'}} after";
        let p = prepare_field(template, None, FieldKind::ContentBlock);
        assert_eq!(
            p.body, "before {{content_blocks.${promo}}} after",
            "cb_id filter must be stripped for new resources"
        );
        assert!(
            !p.additions
                .contains_key(&(PlaceholderType::CbId, "promo".into())),
            "no cb_id addition needed once filter is stripped"
        );
        assert!(p.warnings.iter().any(|w| w.contains("promo")));
    }

    #[test]
    fn lid_without_remote_match_emits_warning_and_no_addition() {
        let template = r#"<a href="https://x.com/cta">__BRAZESYNC.lid.cta__</a>"#;
        let remote = r#"<p>no anchor</p>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(!p
            .additions
            .contains_key(&(PlaceholderType::Lid, "cta".to_string())));
        assert!(p.warnings.iter().any(|w| w.contains("not found")));
    }

    #[test]
    fn vml_href_anchors_lid() {
        // Real templatized form: the lid placeholder is INSIDE the
        // open-tag's href attribute value (matches what templatize
        // emits for `<v:roundrect href="…?lid={{x | lid: 'X'}}">`).
        let template = r#"<v:roundrect href="https://x.com/page/?lid={{x | lid: '__BRAZESYNC.lid.page__'}}">label</v:roundrect>"#;
        let remote = r#"<v:roundrect href="https://x.com/page/?lid={{x | lid: 'liveeeeeeee1'}}">label</v:roundrect>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert_eq!(
            p.additions[&(PlaceholderType::Lid, "page".into())],
            "liveeeeeeee1"
        );
    }

    #[test]
    fn plaintext_url_anchor_matches() {
        let template = "Visit https://x.com/cta __BRAZESYNC.lid.cta__ now";
        let remote = "Visit https://x.com/cta {{x | lid: 'liveeeeeeee1'}} now";
        let p = prepare_field(template, Some(remote), FieldKind::EmailPlainBody);
        assert_eq!(
            p.additions[&(PlaceholderType::Lid, "cta".into())],
            "liveeeeeeee1"
        );
    }
}
