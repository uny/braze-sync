//! Migration pass: raw-lid / raw-cb_id bodies → templated bodies + values.
//!
//! Powers `braze-sync templatize` (RFC §2.7). All functions in this
//! module are pure — they take a body string + field kind, and return
//! the rewritten body together with the per-occurrence detection
//! metadata the CLI orchestrator uses to populate values files.

use regex_lite::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::values::correlation::{normalize_url, slug_for_cb_id, slug_for_lid};

/// Which Liquid context the body belongs to. Determines:
/// - what kind of URL anchor lid detection should look for (HTML vs raw)
/// - whether lid detection without a URL anchor should produce a
///   sequential `link_N` key (deferred for subject/preheader v0.14)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    ContentBlock,
    EmailHtmlBody,
    EmailPlainBody,
    EmailSubject,
    EmailPreheader,
}

impl FieldKind {
    pub fn supports_html_anchor(self) -> bool {
        matches!(self, FieldKind::ContentBlock | FieldKind::EmailHtmlBody)
    }
    pub fn supports_plaintext_anchor(self) -> bool {
        matches!(self, FieldKind::EmailPlainBody)
    }
}

/// One placeholder produced by templatization, with the metadata the
/// caller needs to update `values/<env>.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedEntry {
    Lid {
        key: String,
        value: String,
        /// Normalized URL anchor, when this field has one. `None` for
        /// subject/preheader where lid auto-detection currently falls
        /// back to a sequential `link_N` key (no URL to anchor on).
        url: Option<String>,
    },
    CbId {
        key: String,
        value: String,
        /// The original Liquid `${NAME}` identifier; recorded for
        /// debugging and so the slug round-trips back.
        name: String,
    },
}

impl DetectedEntry {
    pub fn key(&self) -> &str {
        match self {
            DetectedEntry::Lid { key, .. } | DetectedEntry::CbId { key, .. } => key,
        }
    }
}

/// Result of templatizing one body field.
#[derive(Debug, Clone)]
pub struct TemplatizedField {
    pub new_body: String,
    pub entries: Vec<DetectedEntry>,
    /// Warnings the CLI should surface (e.g. lid in subject/preheader
    /// where we don't have a robust anchor).
    pub warnings: Vec<String>,
}

/// Detect every `| lid: '<value>'` and `{{content_blocks.${NAME} | id: 'cbN'}}`
/// in `body`, rewrite to `__BRAZESYNC.<type>.<key>__` placeholders,
/// and return the rewritten body together with the per-occurrence
/// detection metadata. Idempotent by construction: detection regexes
/// require raw lid (`[a-z0-9]{8,}`) / cb_id (`cb[0-9]+`) literals, so
/// already-templated `__BRAZESYNC.*__` placeholders never re-match.
/// This means a partially-templatized body (existing placeholders
/// alongside remaining raw values) still gets the raw values picked up,
/// instead of being silently skipped.
pub fn templatize_body(body: &str, field: FieldKind) -> TemplatizedField {
    let mut spans: Vec<DetectionSpan> = Vec::new();
    // Order matters per RFC §3 Q3 connumber fallback: detect lids in
    // appearance order, dedup keys by sequential suffix.
    let mut used_lid_keys: BTreeMap<String, usize> = BTreeMap::new();
    let mut used_cb_id_keys: BTreeMap<String, usize> = BTreeMap::new();
    // Repeated `${NAME}` cb_id references must reuse the same key so
    // export refresh (which correlates by NAME) can match every
    // occurrence. Without this, the second `${promo}` would slug to
    // `promo_2` and refresh would never find a remote match.
    let mut cb_id_name_to_key: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();

    // --- lid detection ---
    for m in lid_match_re().captures_iter(body) {
        let whole = m.get(0).expect("group 0 always present");
        let value = m
            .get(1)
            .or(m.get(2))
            .map(|g| g.as_str().to_string())
            .expect("one of the value alternates matches");

        let (url, key) = name_lid_for_field(body, whole.start(), field, &mut used_lid_keys);
        if url.is_none() && !matches!(field, FieldKind::EmailSubject | FieldKind::EmailPreheader) {
            warnings.push(format!(
                "lid '{value}' at byte {} has no URL anchor; using sequential key '{key}'",
                whole.start()
            ));
        }
        if matches!(field, FieldKind::EmailSubject | FieldKind::EmailPreheader) {
            // Phase 3 export does NOT refresh subject/preheader lid
            // entries (see exporter.rs refresh path). Skeleton files
            // produced for other envs will therefore stay `value: null`
            // until manually edited. Surface this once per detection so
            // the operator knows the canonical/skeleton gap exists.
            warnings.push(format!(
                "lid '{value}' detected in subject/preheader (key '{key}'); \
                 `export` does not refresh these — non-canonical env \
                 values files must be edited manually"
            ));
        }
        spans.push(DetectionSpan {
            range: whole.range(),
            replacement: format!("| lid: '__BRAZESYNC.lid.{key}__'"),
            entry: DetectedEntry::Lid { key, value, url },
        });
    }

    // --- cb_id detection ---
    for m in cb_id_match_re().captures_iter(body) {
        let whole = m.get(0).expect("group 0 always present");
        let name = m.get(1).expect("name capture present").as_str().to_string();
        let value = m
            .get(2)
            .or(m.get(3))
            .map(|g| g.as_str().to_string())
            .expect("cbN capture present");
        // Same `${NAME}` referenced twice in one body → reuse the
        // first key so export refresh matches every occurrence.
        let key = match cb_id_name_to_key.get(&name) {
            Some(prior) => prior.clone(),
            None => {
                let k = unique_key(slug_for_cb_id(&name), &mut used_cb_id_keys);
                cb_id_name_to_key.insert(name.clone(), k.clone());
                k
            }
        };
        // Preserve the original `${NAME}` form so cb_id correlation in
        // export keeps working.
        let replacement =
            format!("{{{{content_blocks.${{{name}}} | id: '__BRAZESYNC.cb_id.{key}__'}}}}");
        spans.push(DetectionSpan {
            range: whole.range(),
            replacement,
            entry: DetectedEntry::CbId { key, value, name },
        });
    }

    // Apply spans back-to-front so earlier byte offsets remain valid.
    spans.sort_by_key(|s| s.range.start);
    let mut new_body = body.to_string();
    let mut entries_in_order: Vec<DetectedEntry> = Vec::with_capacity(spans.len());
    for s in &spans {
        entries_in_order.push(s.entry.clone());
    }
    for s in spans.into_iter().rev() {
        new_body.replace_range(s.range, &s.replacement);
    }

    TemplatizedField {
        new_body,
        entries: entries_in_order,
        warnings,
    }
}

struct DetectionSpan {
    range: std::ops::Range<usize>,
    replacement: String,
    entry: DetectedEntry,
}

fn lid_match_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // RFC §2.7 step 2: pipe-anchored, dual-quote, min length 8.
        Regex::new(r#"\|\s*lid:\s*(?:"([a-z0-9]{8,})"|'([a-z0-9]{8,})')"#)
            .expect("lid match regex is valid")
    })
}

fn cb_id_match_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\{\{\s*content_blocks\.\$\{\s*([^\s}|]+)\s*\}\s*\|\s*id:\s*(?:"(cb[0-9]+)"|'(cb[0-9]+)')\s*\}\}"#,
        )
        .expect("cb_id match regex is valid")
    })
}

fn href_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)<a\b[^>]*?\bhref\s*=\s*(?:"([^"]*)"|'([^']*)')"#)
            .expect("href regex is valid")
    })
}

fn plaintext_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"https?://[^\s<>"']+"#).expect("plaintext URL regex is valid"))
}

fn name_lid_for_field(
    body: &str,
    lid_token_offset: usize,
    field: FieldKind,
    used: &mut BTreeMap<String, usize>,
) -> (Option<String>, String) {
    let url = preceding_url(body, lid_token_offset, field);
    let key_source: String = match &url {
        Some(u) => url_path_tail(u).to_string(),
        None => String::new(),
    };
    let slug = slug_for_lid(&key_source);
    let key = unique_key(slug, used);
    (url, key)
}

fn preceding_url(body: &str, lid_token_offset: usize, field: FieldKind) -> Option<String> {
    let raw = if field.supports_html_anchor() {
        // Braze's common case puts `{{ ... | lid: '…' }}` *inside* the
        // href attribute value (e.g. `href="…?lid={{…|lid:'X'}}"`).
        // Try the enclosing `<a …>` open tag first; only when the lid
        // lives outside any open tag do we fall back to the last
        // `<a href>` before the token (legacy pattern where lid sits
        // between the `<a>` and `</a>` as link text).
        enclosing_anchor_href(body, lid_token_offset).or_else(|| {
            let prefix = &body[..lid_token_offset];
            href_re()
                .captures_iter(prefix)
                .last()
                .and_then(|cap| cap.get(1).or(cap.get(2)))
                .map(|m| m.as_str().to_string())
        })
    } else if field.supports_plaintext_anchor() {
        let prefix = &body[..lid_token_offset];
        plaintext_url_re()
            .find_iter(prefix)
            .last()
            .map(|m| m.as_str().to_string())
    } else {
        None
    };
    raw.map(|r| normalize_url(&r))
}

/// Find an `<a …>` open tag that *contains* `lid_token_offset` and
/// return its `href` attribute value (unnormalized). Returns `None`
/// when the offset isn't inside any open tag or the enclosing tag has
/// no href.
///
/// Open tags are detected by `<a\b … >` with `>` being the first
/// unmatched `>` after `<a` — same trust assumption as `href_re` (no
/// raw `>` inside attribute values).
fn enclosing_anchor_href(body: &str, lid_token_offset: usize) -> Option<String> {
    let re = anchor_open_tag_re();
    for m in re.find_iter(body) {
        if m.start() > lid_token_offset {
            break;
        }
        if m.end() > lid_token_offset {
            let tag = &body[m.start()..m.end()];
            return href_re()
                .captures(tag)
                .and_then(|cap| cap.get(1).or(cap.get(2)))
                .map(|x| x.as_str().to_string());
        }
    }
    None
}

fn anchor_open_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(?i)<a\b[^>]*>"#).expect("anchor open tag regex is valid"))
}

fn url_path_tail(url: &str) -> String {
    // Strip scheme://host and any leading slashes; take the last
    // non-empty path component. `https://example.com/promo/spring-sale`
    // → `spring-sale`. Bare host or trailing slash → empty (caller
    // applies the `link_` fallback via slug_for_lid).
    let after_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let path_start = after_scheme
        .find('/')
        .map(|i| i + 1)
        .unwrap_or(after_scheme.len());
    let path = &after_scheme[path_start..];
    path.rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

fn unique_key(base: String, used: &mut BTreeMap<String, usize>) -> String {
    let count = used.entry(base.clone()).or_insert(0);
    *count += 1;
    if *count == 1 {
        base
    } else {
        format!("{base}_{count}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotent_on_already_templatized_body() {
        let body = "<p>__BRAZESYNC.lid.cta__ kept verbatim</p>";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.new_body, body);
        assert!(r.entries.is_empty());
    }

    #[test]
    fn rewrites_html_lid_with_url_anchor() {
        let body = r#"<a href="https://example.com/spring-sale">{{x | lid: 'ai8kexrxcp03'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("__BRAZESYNC.lid.spring_sale__"));
        assert_eq!(r.entries.len(), 1);
        match &r.entries[0] {
            DetectedEntry::Lid { key, value, url } => {
                assert_eq!(key, "spring_sale");
                assert_eq!(value, "ai8kexrxcp03");
                assert_eq!(url.as_deref(), Some("https://example.com/spring-sale"));
            }
            _ => panic!("expected Lid"),
        }
    }

    #[test]
    fn rewrites_cb_id_include() {
        let body = "{{content_blocks.${promo_banner} | id: 'cb42'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("__BRAZESYNC.cb_id.promo_banner__"));
        // Preserves ${NAME} so export correlation still works.
        assert!(r.new_body.contains("${promo_banner}"));
        assert_eq!(r.entries.len(), 1);
    }

    #[test]
    fn dedupes_duplicate_url_with_sequential_suffix() {
        let body = r#"
<a href="https://example.com/cta">{{x | lid: 'ai8kexrxcp03'}}A</a>
<a href="https://example.com/cta">{{x | lid: 'bj9lfsysxq14'}}B</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        let keys: Vec<&str> = r.entries.iter().map(DetectedEntry::key).collect();
        assert_eq!(keys, ["cta", "cta_2"]);
    }

    #[test]
    fn plaintext_url_anchor_works() {
        let body = "Click https://example.com/promo {{x | lid: 'ai8kexrxcp03'}} now.";
        let r = templatize_body(body, FieldKind::EmailPlainBody);
        match &r.entries[0] {
            DetectedEntry::Lid { key, url, .. } => {
                assert_eq!(key, "promo");
                assert_eq!(url.as_deref(), Some("https://example.com/promo"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn subject_lid_warns_about_export_refresh_gap() {
        // subject has no URL anchor — slug falls back to `link_`. The
        // CLI must surface that `export` won't refresh this entry for
        // other envs so the operator knows to maintain values manually.
        let body = "Hello {{x | lid: 'ai8kexrxcp03'}} world";
        let r = templatize_body(body, FieldKind::EmailSubject);
        assert!(
            r.warnings
                .iter()
                .any(|w| w.contains("export") && w.contains("subject")),
            "expected manual-maintenance warning, got: {:?}",
            r.warnings
        );
        match &r.entries[0] {
            DetectedEntry::Lid { key, url, .. } => {
                assert_eq!(key, "link_");
                assert!(url.is_none());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn repeated_cb_id_name_reuses_key() {
        // RFC: same `${NAME}` resolves to the same content_block. The
        // values file must have ONE entry for it, not `name` + `name_2`,
        // otherwise export refresh can never populate the duplicates.
        let body = "{{content_blocks.${promo} | id: 'cb10'}} ... \
                    {{content_blocks.${promo} | id: 'cb10'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.entries.len(), 2, "both occurrences detected");
        assert_eq!(r.entries[0].key(), "promo");
        assert_eq!(
            r.entries[1].key(),
            "promo",
            "same ${{NAME}} must reuse the key"
        );
    }

    #[test]
    fn partially_templatized_body_picks_up_remaining_raw_lid() {
        // Mixed state: one lid already templated, another still raw.
        // The raw one MUST be detected (no early-return short-circuit).
        let body = r#"
<a href="https://example.com/cta">{{ x | lid: '__BRAZESYNC.lid.cta__' }}A</a>
<a href="https://example.com/promo">{{ x | lid: 'rawvalue1234' }}B</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.entries.len(), 1, "the raw lid must be detected");
        match &r.entries[0] {
            DetectedEntry::Lid { key, value, .. } => {
                assert_eq!(key, "promo");
                assert_eq!(value, "rawvalue1234");
            }
            _ => panic!("expected Lid"),
        }
    }

    #[test]
    fn html_lid_without_anchor_warns() {
        // HTML body but the lid has no preceding <a href> — RFC says
        // this should still produce a key but flag it for the operator.
        let body = "{{x | lid: 'ai8kexrxcp03'}} just floating";
        let r = templatize_body(body, FieldKind::EmailHtmlBody);
        assert_eq!(r.entries.len(), 1);
        assert!(!r.warnings.is_empty());
    }

    #[test]
    fn lid_inside_href_attribute_value_uses_enclosing_anchor() {
        // Braze's typical HTML output puts the lid *inside* the href:
        //   <a href="https://…/path/?lid={{${cblid} | lid: 'X'}}">
        // The prefix-only scan can't see the closing quote and was
        // falling back to a sequential `link_` key for ~all anchors.
        let body = r#"<a href="https://med.example.com/product/jaypirca/50mg/?lid={{${cblid} | lid: 'ai8kexrxcp03'}}"><img src="x"/></a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.entries.len(), 1);
        match &r.entries[0] {
            DetectedEntry::Lid { key, url, .. } => {
                assert_eq!(key, "link_50mg");
                assert_eq!(
                    url.as_deref(),
                    Some("https://med.example.com/product/jaypirca/50mg/")
                );
            }
            _ => panic!("expected Lid"),
        }
        assert!(
            r.warnings.is_empty(),
            "no-anchor warning should not fire when href encloses the lid"
        );
    }

    #[test]
    fn enclosing_anchor_takes_precedence_over_earlier_unrelated_href() {
        // Even if an earlier, fully-closed <a href> exists, the lid
        // that lives inside a *different* later <a …> tag should use
        // that later tag's href, not the prior one.
        let body = r#"<a href="https://example.com/old">old</a> then <a href="https://example.com/new/path/?lid={{x | lid: 'ai8kexrxcp03'}}">new</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        match &r.entries[0] {
            DetectedEntry::Lid { url, .. } => {
                assert_eq!(url.as_deref(), Some("https://example.com/new/path/"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn enclosing_anchor_without_href_falls_back_to_prior_href() {
        // The lid lives inside an `<a>` open tag that has no `href`
        // (e.g. `<a name="…">`). `enclosing_anchor_href` finds the
        // enclosing tag but returns None because there's no href to
        // extract — the legacy prefix scan must still pick up the
        // earlier `<a href>` so we don't regress that pattern.
        let body = r#"<a href="https://example.com/earlier/path">x</a> <a name="anchor">text {{x | lid: 'ai8kexrxcp03'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.entries.len(), 1);
        match &r.entries[0] {
            DetectedEntry::Lid { url, .. } => {
                assert_eq!(url.as_deref(), Some("https://example.com/earlier/path"));
            }
            _ => panic!("expected Lid"),
        }
    }

    #[test]
    fn url_path_tail_uses_last_nonempty_segment() {
        assert_eq!(
            url_path_tail("https://example.com/promo/spring-sale"),
            "spring-sale"
        );
        assert_eq!(url_path_tail("https://example.com/"), "");
        assert_eq!(url_path_tail("https://example.com"), "");
    }
}
