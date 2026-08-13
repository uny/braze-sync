//! Runtime resolution of anonymous `__BRAZESYNC__` placeholders.
//!
//! Resolved at apply/diff time from the remote body via URL / `${NAME}`
//! anchor correlation. Type (`lid` vs `cb_id`) is inferred from the
//! surrounding `| lid:` / `| id:` filter syntax.
//!
//! New-resource fallback (no remote):
//! - **lid**: URL path tail slug used as the value; Braze reassigns on
//!   first dashboard open.
//! - **cb_id**: `| id: '…'` filter stripped; Braze derives internally.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex_lite::Regex;

use crate::values::correlation::{
    extract_cb_id_values, extract_html_lid_values, extract_lid_values_unanchored,
    extract_plaintext_lid_values, normalize_url, plaintext_url_anchors, slug_for_lid,
    CbIdCorrelation, LidCorrelation, MANAGED_FILTER_MASK,
};
use crate::values::placeholder::{
    extract_placeholders, find_suspicious_placeholders, PlaceholderType, ResolutionError, TOKEN,
};
use crate::values::templatize::FieldKind;

/// Per-field result of resolving a templatized body.
#[derive(Debug, Clone)]
pub struct PreparedTemplate {
    /// Fully-resolved body. When `errors` is non-empty some `__BRAZESYNC__`
    /// tokens may still be present — the caller treats the field as
    /// failed and surfaces the errors.
    pub body: String,
    /// Unresolved placeholders, retired-namespace tokens, etc.
    pub errors: Vec<ResolutionError>,
    /// Non-fatal warnings — ambiguous URL matches, count mismatches,
    /// stripped cb_id filters on new resources.
    pub warnings: Vec<String>,
    /// Drift-fallback `lid` values: template placeholders that had no
    /// matching remote anchor and were resolved with a generated slug.
    /// Brand-new-resource fallbacks are *not* recorded here — they are
    /// the expected path and would be noise. Populated only when a
    /// remote body was provided but came up short.
    pub fallbacks: Vec<LidFallback>,
}

/// A single drift-fallback assignment. `anchor` is the URL anchor when
/// available (HTML / plaintext fields); `None` for positional contexts
/// like subject / preheader where the placeholder has no URL anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LidFallback {
    pub anchor: Option<String>,
    pub value: String,
}

/// Resolve every `__BRAZESYNC__` in `template` against `remote`.
///
/// `remote = None` means the resource does not yet exist in Braze
/// (new-resource path: lid → slug fallback, cb_id → filter stripped).
pub fn prepare_field(template: &str, remote: Option<&str>, field: FieldKind) -> PreparedTemplate {
    let mut errors: Vec<ResolutionError> = Vec::new();

    // Retired v0.15 envelope detection — fatal so operators re-run
    // `templatize`.
    for tok in find_suspicious_placeholders(template) {
        errors.push(ResolutionError::RetiredNamespace { token: tok });
    }

    if !template.contains(TOKEN) {
        return PreparedTemplate {
            body: template.to_string(),
            errors,
            warnings: Vec::new(),
            fallbacks: Vec::new(),
        };
    }

    let (body, mut warnings) = match remote {
        Some(_) => (template.to_string(), Vec::new()),
        None => strip_cb_id_filters(template),
    };
    let mut fallbacks: Vec<LidFallback> = Vec::new();

    // Map each `__BRAZESYNC__` occurrence in `body` to its resolved
    // value. None entries are recorded as errors instead.
    let placeholders = extract_placeholders(&body);
    let mut resolved: Vec<(usize, usize, Option<String>)> = Vec::new();

    // Collect lid placeholders so we can do the URL-bucket FIFO match in
    // one pass.
    let lid_indices: Vec<usize> = placeholders
        .iter()
        .enumerate()
        .filter(|(_, p)| p.ty == Some(PlaceholderType::Lid))
        .map(|(i, _)| i)
        .collect();

    let lid_values: Vec<Option<String>> = match remote {
        Some(remote_body) => resolve_lid_batch(
            &body,
            &placeholders,
            &lid_indices,
            remote_body,
            field,
            &mut warnings,
            &mut fallbacks,
        ),
        None => fallback_lid_batch(&body, &placeholders, &lid_indices, field),
    };

    // cb_id resolution map (offset → value or None).
    let cb_id_resolved: BTreeMap<usize, Option<String>> = match remote {
        Some(remote_body) => resolve_cb_id_batch(&body, &placeholders, remote_body, &mut warnings),
        None => BTreeMap::new(),
    };

    let mut lid_iter = lid_values.into_iter();
    for ph in &placeholders {
        match ph.ty {
            None => {
                errors.push(ResolutionError::UnknownContext { start: ph.start });
                resolved.push((ph.start, ph.end, None));
            }
            Some(PlaceholderType::Lid) => {
                let v = lid_iter.next().flatten();
                if v.is_none() {
                    let anchor = lid_anchor_for(&body, ph.start, field);
                    errors.push(ResolutionError::UnresolvedLid {
                        start: ph.start,
                        anchor,
                    });
                }
                resolved.push((ph.start, ph.end, v));
            }
            Some(PlaceholderType::CbId) => {
                let v = cb_id_resolved.get(&ph.start).cloned().flatten();
                if v.is_none() {
                    let name = cb_id_name_at(&body, ph.start);
                    errors.push(ResolutionError::UnresolvedCbId {
                        start: ph.start,
                        name,
                    });
                }
                resolved.push((ph.start, ph.end, v));
            }
        }
    }

    // Substitute back-to-front so byte offsets stay valid.
    let mut out = body;
    for (start, end, value) in resolved.into_iter().rev() {
        if let Some(v) = value {
            out.replace_range(start..end, &v);
        }
    }

    PreparedTemplate {
        body: out,
        errors,
        warnings,
        fallbacks,
    }
}

/// Resolve lid placeholders against `remote`. Returns one entry per
/// lid placeholder in template-appearance order.
fn resolve_lid_batch(
    body: &str,
    placeholders: &[crate::values::placeholder::Placeholder],
    lid_indices: &[usize],
    remote: &str,
    field: FieldKind,
    warnings: &mut Vec<String>,
    fallbacks: &mut Vec<LidFallback>,
) -> Vec<Option<String>> {
    if lid_indices.is_empty() {
        return Vec::new();
    }
    if !field.supports_html_anchor() && !field.supports_plaintext_anchor() {
        return resolve_lid_positional(
            placeholders,
            lid_indices,
            remote,
            field,
            warnings,
            fallbacks,
        );
    }

    let remote_pairs: Vec<LidCorrelation> = if field.supports_html_anchor() {
        extract_html_lid_values(remote)
    } else {
        extract_plaintext_lid_values(remote)
    };

    // Each lid placeholder's anchor URL in template-appearance order.
    let anchors: Vec<Option<String>> = lid_indices
        .iter()
        .map(|&i| lid_anchor_for(body, placeholders[i].start, field))
        .collect();

    let mut by_url: BTreeMap<String, std::collections::VecDeque<&LidCorrelation>> = BTreeMap::new();
    for p in &remote_pairs {
        by_url.entry(p.url.clone()).or_default().push_back(p);
    }

    let mut tmpl_per_url: BTreeMap<String, usize> = BTreeMap::new();
    for u in anchors.iter().flatten() {
        *tmpl_per_url.entry(u.clone()).or_insert(0) += 1;
    }
    for (url, bucket) in &by_url {
        let tmpl_count = tmpl_per_url.get(url).copied().unwrap_or(0);
        if bucket.len() > 1 || (tmpl_count > 0 && bucket.len() != tmpl_count) {
            warnings.push(format!(
                "URL '{url}' has {} remote lid occurrences and {tmpl_count} \
                 template placeholders — using positional FIFO match. \
                 If links were reordered in Braze, lid values may be assigned \
                 to the wrong placeholder.",
                bucket.len()
            ));
        }
    }

    let mut out = Vec::with_capacity(lid_indices.len());
    // Seed `used` with every remote lid value so a fallback slug can
    // never duplicate a value that is already in the POSTed body. Braze
    // treats `lid` as a per-link identifier; duplicates corrupt link
    // analytics.
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    for p in &remote_pairs {
        used.entry(p.value.clone()).or_insert(1);
    }
    let mut seq = 0usize;
    for anchor in anchors {
        let Some(url) = anchor else {
            warnings.push(
                "lid placeholder has no URL anchor in template — \
                 anchor-less correlation is not supported; resolve will fail"
                    .to_string(),
            );
            out.push(None);
            continue;
        };
        let pick = by_url.get_mut(&url).and_then(|b| b.pop_front());
        match pick {
            Some(p) => out.push(Some(p.value.clone())),
            None => {
                let fallback = fallback_lid_for_url(Some(&url), &mut used, &mut seq);
                warnings.push(format!(
                    "lid: URL anchor '{url}' not found in remote body — \
                     using fallback value '{fallback}' (new link; Braze will \
                     reassign on first dashboard save)"
                ));
                fallbacks.push(LidFallback {
                    anchor: Some(url.clone()),
                    value: fallback.clone(),
                });
                out.push(Some(fallback));
            }
        }
    }
    out
}

/// Slug fallback for a single lid placeholder. `used` is shared across
/// the batch so collisions are disambiguated with `_2`, `_3`, ….
fn fallback_lid_for_url(
    url: Option<&str>,
    used: &mut BTreeMap<String, usize>,
    seq: &mut usize,
) -> String {
    let base = match url {
        Some(u) => {
            let tail = url_path_tail(u);
            let slug = slug_for_lid(&tail);
            if slug.is_empty() {
                *seq += 1;
                format!("lid_{seq}", seq = *seq)
            } else {
                slug
            }
        }
        None => {
            *seq += 1;
            format!("lid_{seq}", seq = *seq)
        }
    };
    unique(base, used)
}

/// Positional FIFO match for subject / preheader.
fn resolve_lid_positional(
    placeholders: &[crate::values::placeholder::Placeholder],
    lid_indices: &[usize],
    remote: &str,
    field: FieldKind,
    warnings: &mut Vec<String>,
    fallbacks: &mut Vec<LidFallback>,
) -> Vec<Option<String>> {
    let remote_values = extract_lid_values_unanchored(remote);
    let field_label = match field {
        FieldKind::EmailSubject => "subject",
        FieldKind::EmailPreheader => "preheader",
        _ => "field",
    };
    if remote_values.len() > lid_indices.len() {
        warnings.push(format!(
            "{field_label} has {} lid placeholder(s) but remote body has {} lid value(s); \
             extra remote values will be dropped — review rendered output",
            lid_indices.len(),
            remote_values.len()
        ));
    }
    let _ = placeholders;
    let mut out = Vec::with_capacity(lid_indices.len());
    // Seed `used` with every remote positional value so fallback
    // slugs (`lid_1`, …) can never collide with a real remote value.
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    for v in &remote_values {
        used.entry(v.clone()).or_insert(1);
    }
    let mut iter = remote_values.into_iter();
    let mut seq = 0usize;
    for _ in lid_indices {
        match iter.next() {
            Some(v) => out.push(Some(v)),
            None => {
                let v = fallback_lid_for_url(None, &mut used, &mut seq);
                fallbacks.push(LidFallback {
                    anchor: None,
                    value: v.clone(),
                });
                out.push(Some(v));
            }
        }
    }
    out
}

/// Pair cb_id placeholders with remote `${NAME} | id: 'cbN'` matches by
/// `${NAME}`. Returns an offset → value map.
fn resolve_cb_id_batch(
    body: &str,
    placeholders: &[crate::values::placeholder::Placeholder],
    remote: &str,
    warnings: &mut Vec<String>,
) -> BTreeMap<usize, Option<String>> {
    let remote_pairs = extract_cb_id_values(remote);
    let remote_by_name: BTreeMap<&str, &CbIdCorrelation> =
        remote_pairs.iter().map(|p| (p.name.as_str(), p)).collect();

    let mut out: BTreeMap<usize, Option<String>> = BTreeMap::new();
    for ph in placeholders {
        if ph.ty != Some(PlaceholderType::CbId) {
            continue;
        }
        let name = match cb_id_name_at(body, ph.start) {
            Some(n) => n,
            None => {
                warnings.push(format!(
                    "cb_id: `__BRAZESYNC__` at byte {} not inside `{{{{content_blocks.${{NAME}} | id: '…'}}}}` — cannot correlate",
                    ph.start
                ));
                out.insert(ph.start, None);
                continue;
            }
        };
        match remote_by_name.get(name.as_str()) {
            Some(pick) => {
                out.insert(ph.start, Some(pick.value.clone()));
            }
            None => {
                warnings.push(format!(
                    "cb_id: `${{{name}}}` include not found in remote body"
                ));
                out.insert(ph.start, None);
            }
        }
    }
    out
}

/// Generate fallback lid values for the new-resource path. Uses URL
/// path tail slug; collisions are disambiguated with `_2`, `_3`, ….
fn fallback_lid_batch(
    body: &str,
    placeholders: &[crate::values::placeholder::Placeholder],
    lid_indices: &[usize],
    field: FieldKind,
) -> Vec<Option<String>> {
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    let mut seq = 0usize;
    let mut out = Vec::with_capacity(lid_indices.len());
    for &i in lid_indices {
        let anchor = lid_anchor_for(body, placeholders[i].start, field);
        out.push(Some(fallback_lid_for_url(
            anchor.as_deref(),
            &mut used,
            &mut seq,
        )));
    }
    out
}

fn unique(base: String, used: &mut BTreeMap<String, usize>) -> String {
    let count = used.entry(base.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base
    } else {
        format!("{base}_{count}")
    }
}

fn url_path_tail(url: &str) -> String {
    // The input is an anchor *key*, so it may carry `MANAGED_FILTER_MASK`
    // — a comparison-only sentinel. The tail feeds `slug_for_lid`, whose
    // output is POSTed as a live Braze `lid`, so strip it first or the
    // slug reads `…_braze_managed`. Now that the plaintext run spans a
    // whole `{{…}}`, the mask lands inside the key on that path too.
    let url = &url.replace(MANAGED_FILTER_MASK, "");
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path_start = after_scheme
        .find('/')
        .map(|i| i + 1)
        .unwrap_or(after_scheme.len());
    // Strip query / fragment (normalize_url already does this for the
    // main call-path, but be safe if the function is reused).
    let path = after_scheme[path_start..]
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Strip `| id: '__BRAZESYNC__'` filters from a template body. Used
/// for the new-resource fallback so we POST the documented
/// `{{content_blocks.${NAME}}}` form.
fn strip_cb_id_filters(body: &str) -> (String, Vec<String>) {
    let re = cb_id_filter_re();
    let mut warnings: Vec<String> = Vec::new();
    let mut spans: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for cap in re.captures_iter(body) {
        let whole = cap.get(0).expect("group 0 always present");
        let name = cap
            .get(1)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        warnings.push(format!(
            "cb_id `${{{name}}}`: new resource — stripping `| id: '…'` filter; \
             Braze will assign a cb_id on first save"
        ));
        spans.push((whole.range(), format!("{{{{content_blocks.${{{name}}}}}}}")));
    }
    let mut out = body.to_string();
    for (range, replacement) in spans.into_iter().rev() {
        out.replace_range(range, &replacement);
    }
    (out, warnings)
}

fn cb_id_filter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Captures `${NAME}` so we can re-emit the documented form
        // (`{{content_blocks.${NAME}}}`) without the cb_id filter.
        Regex::new(
            r#"\{\{\s*content_blocks\.\$\{\s*([^\s}|]+)\s*\}\s*\|\s*id:\s*['"]__BRAZESYNC__['"]\s*\}\}"#,
        )
        .expect("cb_id filter regex is valid")
    })
}

/// Look up the `${NAME}` enclosing a cb_id `__BRAZESYNC__` token.
fn cb_id_name_at(body: &str, offset: usize) -> Option<String> {
    let re = cb_id_template_re();
    for cap in re.captures_iter(body) {
        let whole = cap.get(0)?;
        if whole.start() <= offset && offset < whole.end() {
            return cap.get(1).map(|m| m.as_str().to_string());
        }
    }
    None
}

fn cb_id_template_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\{\{\s*content_blocks\.\$\{\s*([^\s}|]+)\s*\}\s*\|\s*id:\s*['"]__BRAZESYNC__['"]\s*\}\}"#,
        )
        .expect("cb_id template regex is valid")
    })
}

fn lid_anchor_for(body: &str, offset: usize, field: FieldKind) -> Option<String> {
    if field.supports_html_anchor() {
        if let Some(tag) = enclosing_open_tag(body, offset) {
            if let Some(url) = url_attr_re()
                .captures(tag)
                .and_then(|c| c.get(1).or(c.get(2)))
            {
                return Some(normalize_url(url.as_str()));
            }
            return None;
        }
        let prefix = &body[..offset];
        anchor_href_re()
            .captures_iter(prefix)
            .last()
            .and_then(|cap| cap.get(1).or(cap.get(2)))
            .map(|m| normalize_url(m.as_str()))
    } else if field.supports_plaintext_anchor() {
        // The load-bearing part is `plaintext_url_anchors`: the remote
        // side keys through the same trim + normalize, and any asymmetry
        // there makes correlation impossible (see its doc comment).
        //
        // Scanning the whole body — rather than `body[..offset]` — is
        // load-bearing: `plaintext_url_re` spans a whole `{{…}}`, so for a
        // URL assembled from Liquid the run *contains* the placeholder.
        // Truncating at `offset` would cut the tag in half, leaving a
        // partial filter that no longer masks, and the key would go back
        // to depending on where the quote happens to fall. Taking the last
        // URL starting at or before `offset` covers both that case and the
        // usual "URL, then lid tag after it" shape.
        plaintext_url_anchors(body)
            .into_iter()
            .take_while(|(start, _)| *start <= offset)
            .last()
            .map(|(_, url)| url)
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

fn element_open_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)<[a-z][a-z0-9_.:-]*\b[^>]*>"#).expect("element open tag regex is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::values::templatize::templatize_body;

    #[test]
    fn no_placeholders_returns_body_verbatim() {
        let p = prepare_field("<p>hi</p>", Some("<p>hi</p>"), FieldKind::ContentBlock);
        assert_eq!(p.body, "<p>hi</p>");
        assert!(p.errors.is_empty());
    }

    #[test]
    fn html_lid_resolved_via_url_anchor() {
        let template = r#"<a href="https://example.com/cta">{{x | lid: '__BRAZESYNC__'}}</a>"#;
        let remote = r#"<a href="https://example.com/cta">{{x | lid: 'newlidvalue1'}}</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.body.contains("'newlidvalue1'"));
    }

    #[test]
    fn two_lid_placeholders_sharing_one_url_consume_distinct_remote_values() {
        let template = r#"<a href="https://x.com/a">{{x | lid: '__BRAZESYNC__'}}</a>
<a href="https://x.com/a">{{x | lid: '__BRAZESYNC__'}}</a>"#;
        let remote = r#"<a href="https://x.com/a">{{x | lid: 'firstvalu1a'}}</a>
<a href="https://x.com/a">{{x | lid: 'secondval2b'}}</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.body.contains("'firstvalu1a'"));
        assert!(p.body.contains("'secondval2b'"));
    }

    #[test]
    fn cb_id_resolved_via_name() {
        let template = "{{content_blocks.${promo_banner} | id: '__BRAZESYNC__'}}";
        let remote = "{{content_blocks.${promo_banner} | id: 'cb99'}}";
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty());
        assert!(p.body.contains("'cb99'"));
    }

    #[test]
    fn new_resource_lid_uses_url_slug_fallback() {
        let template = r#"<a href="https://x.com/spring-sale">{{x | lid: '__BRAZESYNC__'}}</a>"#;
        let p = prepare_field(template, None, FieldKind::ContentBlock);
        assert!(p.errors.is_empty());
        assert!(p.body.contains("'spring_sale'"), "got: {}", p.body);
    }

    #[test]
    fn new_resource_lid_without_anchor_uses_sequential() {
        let template = "no anchor {{x | lid: '__BRAZESYNC__'}} mid {{x | lid: '__BRAZESYNC__'}}";
        let p = prepare_field(template, None, FieldKind::EmailSubject);
        assert!(p.body.contains("'lid_1'"));
        assert!(p.body.contains("'lid_2'"));
    }

    #[test]
    fn new_resource_strips_cb_id_filter() {
        let template = "before {{content_blocks.${promo} | id: '__BRAZESYNC__'}} after";
        let p = prepare_field(template, None, FieldKind::ContentBlock);
        assert_eq!(p.body, "before {{content_blocks.${promo}}} after");
        assert!(p.warnings.iter().any(|w| w.contains("promo")));
    }

    #[test]
    fn lid_without_remote_match_falls_back_to_slug() {
        let template = r#"<a href="https://x.com/cta">{{x | lid: '__BRAZESYNC__'}}</a>"#;
        let remote = r#"<p>no anchor</p>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.body.contains("'cta'"), "got: {}", p.body);
        assert!(p
            .warnings
            .iter()
            .any(|w| w.contains("not found in remote body")));
    }

    #[test]
    fn template_with_more_lids_than_remote_resolves_extras_via_fallback() {
        let template = r#"<a href="https://x.com/a">{{x | lid: '__BRAZESYNC__'}}</a>
<a href="https://x.com/b">{{x | lid: '__BRAZESYNC__'}}</a>
<a href="https://x.com/c">{{x | lid: '__BRAZESYNC__'}}</a>"#;
        let remote = r#"<a href="https://x.com/a">{{x | lid: 'remoteval1a'}}</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.body.contains("'remoteval1a'"));
        assert!(p.body.contains("'b'"), "got: {}", p.body);
        assert!(p.body.contains("'c'"), "got: {}", p.body);
    }

    #[test]
    fn subject_with_more_lids_than_remote_falls_back() {
        let template = "{{x | lid: '__BRAZESYNC__'}} A {{y | lid: '__BRAZESYNC__'}}";
        let remote = "{{x | lid: 'firstval123'}} A";
        let p = prepare_field(template, Some(remote), FieldKind::EmailSubject);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.body.contains("'firstval123'"));
        assert!(p.body.contains("'lid_1'"), "got: {}", p.body);
    }

    #[test]
    fn retired_envelope_is_fatal() {
        let template = "stuff __BRAZESYNC.lid.foo__ stuff";
        let p = prepare_field(template, None, FieldKind::ContentBlock);
        assert!(p
            .errors
            .iter()
            .any(|e| matches!(e, ResolutionError::RetiredNamespace { .. })));
    }

    #[test]
    fn unknown_context_is_fatal() {
        let template = "bare __BRAZESYNC__ token";
        let p = prepare_field(template, Some(""), FieldKind::ContentBlock);
        assert!(p
            .errors
            .iter()
            .any(|e| matches!(e, ResolutionError::UnknownContext { .. })));
    }

    #[test]
    fn vml_href_anchors_lid() {
        let template = r#"<v:roundrect href="https://x.com/page/?lid={{x | lid: '__BRAZESYNC__'}}">label</v:roundrect>"#;
        let remote = r#"<v:roundrect href="https://x.com/page/?lid={{x | lid: 'liveeeeeeee1'}}">label</v:roundrect>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.body.contains("'liveeeeeeee1'"));
    }

    #[test]
    fn plaintext_url_anchor_matches() {
        let template = "Visit https://x.com/cta {{x | lid: '__BRAZESYNC__'}} now";
        let remote = "Visit https://x.com/cta {{x | lid: 'liveeeeeeee1'}} now";
        let p = prepare_field(template, Some(remote), FieldKind::EmailPlainBody);
        assert!(p.errors.is_empty());
        assert!(p.body.contains("'liveeeeeeee1'"));
    }

    #[test]
    fn subject_lid_resolves_positionally() {
        let template = "{{x | lid: '__BRAZESYNC__'}} A {{y | lid: '__BRAZESYNC__'}}";
        let remote = "{{x | lid: 'firstval123'}} A {{y | lid: 'secondval2b'}}";
        let p = prepare_field(template, Some(remote), FieldKind::EmailSubject);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.body.contains("'firstval123'"));
        assert!(p.body.contains("'secondval2b'"));
    }

    #[test]
    fn new_resource_plaintext_lid_uses_url_slug() {
        let template = "Visit https://x.com/spring-sale {{x | lid: '__BRAZESYNC__'}} now";
        let p = prepare_field(template, None, FieldKind::EmailPlainBody);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(
            p.body.contains("'spring_sale'"),
            "plaintext URL slug must be used, got: {}",
            p.body
        );
    }

    #[test]
    fn url_fallback_disambiguates_against_remote_slug_collision() {
        // The /checkout URL's natural slug 'checkout' also happens to
        // appear as a remote lid value (for an unrelated /a anchor).
        // The seeded `used` map must force the fallback to 'checkout_2'.
        let template = r#"<a href="https://x.com/a">{{x | lid: '__BRAZESYNC__'}}</a>
<a href="https://x.com/checkout">{{x | lid: '__BRAZESYNC__'}}</a>"#;
        let remote = r#"<a href="https://x.com/a">{{x | lid: 'checkout'}}</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        let count = p.body.matches("'checkout'").count();
        assert_eq!(
            count, 1,
            "remote lid must appear exactly once, got: {}",
            p.body
        );
        assert!(p.body.contains("'checkout_2'"), "got: {}", p.body);
    }

    #[test]
    fn url_path_tail_strips_query_and_fragment() {
        assert_eq!(url_path_tail("https://x.com/page/?utm=1"), "page");
        assert_eq!(url_path_tail("https://x.com/page/#section"), "page");
        assert_eq!(url_path_tail("https://x.com/page/?a=1#b"), "page");
        assert_eq!(url_path_tail("https://x.com/"), "");
        assert_eq!(url_path_tail("https://x.com/sale"), "sale");
    }
    #[test]
    fn liquid_separator_href_lid_correlates() {
        // The query separator comes from Liquid, so the href holds no
        // literal `?` and the lid filter is part of the anchor key on
        // both sides. Without masking, template (`__BRAZESYNC__`) and
        // remote (live value) keys can never be equal and the resource
        // is permanently drifted.
        let template =
            r#"<a href="{{ item.url }}{{ sep }}lid={{ x | lid: '__BRAZESYNC__' }}">go</a>"#;
        let remote = r#"<a href="{{ item.url }}{{ sep }}lid={{ x | lid: 'liveeeeeeee1' }}">go</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(p.fallbacks.is_empty(), "{:?}", p.fallbacks);
        assert!(p.body.contains("'liveeeeeeee1'"), "got: {}", p.body);
    }

    #[test]
    fn cb_id_inside_href_does_not_break_lid_anchor() {
        // A content_block include supplying the URL prefix carries its
        // own Braze-managed value; it must be masked out of the anchor
        // key too, or the lid never correlates.
        let template = r#"<a href="{{content_blocks.${base} | id: '__BRAZESYNC__'}}{{sep}}lid={{x | lid: '__BRAZESYNC__'}}">go</a>"#;
        let remote = r#"<a href="{{content_blocks.${base} | id: 'cb42'}}{{sep}}lid={{x | lid: 'liveeeeeeee1'}}">go</a>"#;
        let p = prepare_field(template, Some(remote), FieldKind::ContentBlock);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(p.body.contains("'liveeeeeeee1'"), "got: {}", p.body);
        assert!(p.body.contains("'cb42'"), "got: {}", p.body);
    }

    #[test]
    fn plaintext_lid_anchor_trims_trailing_punctuation() {
        // Remote-side extraction trims the sentence period; the
        // template-side anchor must trim it identically.
        let template = "See https://x.com/end. {{x | lid: '__BRAZESYNC__'}}";
        let remote = "See https://x.com/end. {{x | lid: 'liveeeeeeee1'}}";
        let p = prepare_field(template, Some(remote), FieldKind::EmailPlainBody);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(p.body.contains("'liveeeeeeee1'"), "got: {}", p.body);
    }

    #[test]
    fn plaintext_lid_inside_liquid_url_correlates() {
        // No literal `?` to cut at, and the run now spans the whole
        // `{{…}}`, so the key is `https://x.com/p{{sep}}lid={{x` plus the
        // masked filter. Masking is what makes template and remote agree
        // here despite `templatize` respacing the filter — each spelling
        // round-trips against its own remote, which is what
        // `plaintext_lid_round_trips_whatever_the_filter_spacing` pins.
        // The two spellings do *not* produce the same key as each other
        // (the space before `|` is outside the mask).
        let template = "Visit https://x.com/p{{sep}}lid={{x|lid:'__BRAZESYNC__'}} now";
        let remote = "Visit https://x.com/p{{sep}}lid={{x|lid:'liveeeeeeee1'}} now";
        let p = prepare_field(template, Some(remote), FieldKind::EmailPlainBody);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(p.body.contains("'liveeeeeeee1'"), "got: {}", p.body);
    }

    #[test]
    fn plaintext_lid_round_trips_whatever_the_filter_spacing() {
        // `templatize` canonicalizes the filter, inserting a space into
        // the compact spelling — so the round-trip is the identity up to
        // that rewrite, i.e. the live value must land back in the token's
        // place. Whichever way the author wrote the filter, resolving the
        // template against the very remote it came from has to recover
        // `liveeeeeeee1`; otherwise every apply overwrites the live lid
        // with a fallback slug and the resource never converges.
        for remote in [
            "Visit https://x.com/p{{sep}}lid={{x|lid:'liveeeeeeee1'}} now",
            "Visit https://x.com/p{{sep}}lid={{x | lid: 'liveeeeeeee1'}} now",
        ] {
            let t = templatize_body(remote, FieldKind::EmailPlainBody);
            assert_eq!(t.lid_rewrites, 1, "not templatized: {}", t.new_body);
            let p = prepare_field(&t.new_body, Some(remote), FieldKind::EmailPlainBody);
            assert!(p.errors.is_empty(), "{:?}", p.errors);
            assert!(p.warnings.is_empty(), "{:?}", p.warnings);
            assert!(p.fallbacks.is_empty(), "{:?}", p.fallbacks);
            assert_eq!(
                p.body,
                t.new_body.replace(TOKEN, "liveeeeeeee1"),
                "the live lid must land back in the token's place"
            );
        }
    }

    #[test]
    fn editing_copy_after_a_plaintext_link_keeps_the_live_lid() {
        // The link is untouched; only the copy behind it changed. The
        // anchor must not depend on that copy — if it does, apply POSTs a
        // fallback slug over `liveeeeeeee1` and the Braze-side click
        // history for the link is severed. Nothing reassigns it back,
        // because the fallback is itself a valid lid.
        let remote = "Visit https://x.com/p{{x | lid: 'liveeeeeeee1'}}.{{ old_copy }}";
        let template = "Visit https://x.com/p{{x | lid: '__BRAZESYNC__'}}.{{ new_copy }}";
        let p = prepare_field(template, Some(remote), FieldKind::EmailPlainBody);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.fallbacks.is_empty(), "{:?}", p.fallbacks);
        assert_eq!(
            p.body, "Visit https://x.com/p{{x | lid: 'liveeeeeeee1'}}.{{ new_copy }}",
            "the live lid must survive an edit to the copy after the link"
        );
    }

    #[test]
    fn plaintext_fallback_slug_never_leaks_the_managed_filter_mask() {
        // The anchor key now spans the whole `{{…}}`, so `normalize_url`
        // masks the filter *inside* it. That mask is a comparison-only
        // sentinel: it must not reach the slug, which is POSTed as a live
        // Braze `lid`. Both fallback paths derive from the same key.
        let template = "Visit https://x.com/promo{{sep}}lid={{x | lid: '__BRAZESYNC__'}} now";
        for remote in [None, Some("Visit https://elsewhere.example/other now")] {
            let p = prepare_field(template, remote, FieldKind::EmailPlainBody);
            assert!(p.errors.is_empty(), "{:?}", p.errors);
            assert!(
                !p.body.contains("braze_managed"),
                "sentinel leaked into the POSTed lid: {}",
                p.body
            );
            assert!(
                p.body.contains("| lid: 'promo_sep_lid_x'}}"),
                "got: {}",
                p.body
            );
        }
    }

    #[test]
    fn plaintext_urls_differing_only_after_the_filter_keep_distinct_anchors() {
        // Both links share a prefix and differ only *past* the lid filter.
        // The remote lists them in the opposite order, so a collapsed key
        // hands each link the other's live lid via FIFO — and because both
        // values stay valid, Braze never self-corrects and click
        // attribution is transposed permanently.
        let template = "Go https://x.com/p{{sep}}lid={{x | lid: '__BRAZESYNC__'}}/beta and \
                        https://x.com/p{{sep}}lid={{x | lid: '__BRAZESYNC__'}}/alpha now";
        let remote = "Go https://x.com/p{{sep}}lid={{x | lid: 'aaaaaaaaaaa'}}/alpha and \
                      https://x.com/p{{sep}}lid={{x | lid: 'bbbbbbbbbbb'}}/beta now";
        let p = prepare_field(template, Some(remote), FieldKind::EmailPlainBody);
        assert!(p.errors.is_empty(), "{:?}", p.errors);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert!(
            p.body.contains("lid: 'bbbbbbbbbbb'}}/beta"),
            "/beta must keep its own lid, got: {}",
            p.body
        );
        assert!(
            p.body.contains("lid: 'aaaaaaaaaaa'}}/alpha"),
            "/alpha must keep its own lid, got: {}",
            p.body
        );
    }
}
