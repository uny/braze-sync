//! Migration pass: raw-lid / raw-cb_id bodies → templated bodies.
//!
//! Powers `braze-sync templatize`. All functions are pure — they take
//! a body string + field kind and return the rewritten body plus any
//! warnings the operator should see.
//!
//! v0.15 model: templatization rewrites raw `| lid: 'X'` and
//! `{{content_blocks.${NAME} | id: 'cbN'}}` to `__BRAZESYNC.*__`
//! placeholders. The raw values are NOT persisted anywhere — at
//! apply/diff time they are re-fetched from the remote body (see
//! [`crate::values::braze_managed`]).

use regex_lite::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

use crate::values::correlation::{normalize_url, slug_for_cb_id, slug_for_lid};

/// Which Liquid context the body belongs to. Determines:
/// - what kind of URL anchor lid detection should look for (HTML vs raw)
/// - whether lid detection without a URL anchor should produce a
///   sequential `link_N` key (deferred for subject/preheader)
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

/// Result of templatizing one body field.
#[derive(Debug, Clone)]
pub struct TemplatizedField {
    pub new_body: String,
    /// How many `| lid: 'X'` occurrences were rewritten.
    pub lid_rewrites: usize,
    /// How many `{{content_blocks.${NAME} | id: 'cbN'}}` occurrences
    /// were rewritten.
    pub cb_id_rewrites: usize,
    /// Warnings the CLI should surface (e.g. lid in subject/preheader
    /// where we don't have a robust anchor).
    pub warnings: Vec<String>,
}

/// Detect every `| lid: '<value>'` and `{{content_blocks.${NAME} | id: 'cbN'}}`
/// in `body`, rewrite to `__BRAZESYNC.<type>.<key>__` placeholders,
/// and return the rewritten body plus a count of each rewrite kind.
/// Idempotent by construction: the detection regexes require raw
/// literals (`[a-z0-9]{8,}` for lid, `cb[0-9]+` for cb_id), so an
/// already-templated `__BRAZESYNC.*__` placeholder never re-matches.
pub fn templatize_body(body: &str, field: FieldKind) -> TemplatizedField {
    let mut spans: Vec<DetectionSpan> = Vec::new();
    let mut used_lid_keys: BTreeMap<String, usize> = BTreeMap::new();
    let mut used_cb_id_keys: BTreeMap<String, usize> = BTreeMap::new();
    // Repeated `${NAME}` cb_id references reuse the same key.
    let mut cb_id_name_to_key: BTreeMap<String, String> = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut lid_rewrites = 0usize;
    let mut cb_id_rewrites = 0usize;

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
            warnings.push(format!(
                "lid '{value}' detected in subject/preheader (key '{key}'); \
                 resolved positionally at apply/diff time (Nth placeholder = Nth remote lid). \
                 Verify rendered output if the field contains multiple lid values."
            ));
        }
        spans.push(DetectionSpan {
            range: whole.range(),
            replacement: format!("| lid: '__BRAZESYNC.lid.{key}__'"),
        });
        lid_rewrites += 1;
        let _ = value; // value no longer persisted; surface in warning above when relevant.
    }

    // --- cb_id detection ---
    for m in cb_id_match_re().captures_iter(body) {
        let whole = m.get(0).expect("group 0 always present");
        let name = m.get(1).expect("name capture present").as_str().to_string();
        let _value = m
            .get(2)
            .or(m.get(3))
            .map(|g| g.as_str().to_string())
            .expect("cbN capture present");
        let key = match cb_id_name_to_key.get(&name) {
            Some(prior) => prior.clone(),
            None => {
                let k = unique_key(slug_for_cb_id(&name), &mut used_cb_id_keys);
                cb_id_name_to_key.insert(name.clone(), k.clone());
                k
            }
        };
        let replacement =
            format!("{{{{content_blocks.${{{name}}} | id: '__BRAZESYNC.cb_id.{key}__'}}}}");
        spans.push(DetectionSpan {
            range: whole.range(),
            replacement,
        });
        cb_id_rewrites += 1;
    }

    spans.sort_by_key(|s| s.range.start);
    let mut new_body = body.to_string();
    for s in spans.into_iter().rev() {
        new_body.replace_range(s.range, &s.replacement);
    }

    TemplatizedField {
        new_body,
        lid_rewrites,
        cb_id_rewrites,
        warnings,
    }
}

struct DetectionSpan {
    range: std::ops::Range<usize>,
    replacement: String,
}

fn lid_match_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
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

fn name_lid_for_field(
    body: &str,
    lid_token_offset: usize,
    field: FieldKind,
    used: &mut BTreeMap<String, usize>,
) -> (Option<String>, String) {
    let url = preceding_url(body, lid_token_offset, field);
    // Subject/preheader have no anchors; use a stable field-scoped base
    // so the generated keys are self-describing rather than the generic
    // `link` fallback shared with anchor-less HTML.
    let key_source: String = match (&url, field) {
        (Some(u), _) => url_path_tail(u),
        (None, FieldKind::EmailSubject) => "subject_lid".to_string(),
        (None, FieldKind::EmailPreheader) => "preheader_lid".to_string(),
        (None, _) => String::new(),
    };
    let slug = slug_for_lid(&key_source);
    let key = unique_key(slug, used);
    (url, key)
}

fn preceding_url(body: &str, lid_token_offset: usize, field: FieldKind) -> Option<String> {
    let raw = if field.supports_html_anchor() {
        match enclosing_open_tag(body, lid_token_offset) {
            Some(tag) => url_attr_re()
                .captures(tag)
                .and_then(|cap| cap.get(1).or(cap.get(2)))
                .map(|x| x.as_str().to_string()),
            None => {
                let prefix = &body[..lid_token_offset];
                anchor_href_re()
                    .captures_iter(prefix)
                    .last()
                    .and_then(|cap| cap.get(1).or(cap.get(2)))
                    .map(|m| m.as_str().to_string())
            }
        }
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

fn enclosing_open_tag(body: &str, lid_token_offset: usize) -> Option<&str> {
    let re = element_open_tag_re();
    for m in re.find_iter(body) {
        if m.start() > lid_token_offset {
            break;
        }
        if m.end() > lid_token_offset {
            return Some(&body[m.start()..m.end()]);
        }
    }
    None
}

fn element_open_tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)<[a-z][a-z0-9_.:-]*\b[^>]*>"#).expect("element open tag regex is valid")
    })
}

fn url_path_tail(url: &str) -> String {
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
        assert_eq!(r.lid_rewrites, 0);
    }

    #[test]
    fn rewrites_html_lid_with_url_anchor() {
        let body = r#"<a href="https://example.com/spring-sale">{{x | lid: 'ai8kexrxcp03'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("__BRAZESYNC.lid.spring_sale__"));
        assert_eq!(r.lid_rewrites, 1);
    }

    #[test]
    fn rewrites_cb_id_include() {
        let body = "{{content_blocks.${promo_banner} | id: 'cb42'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("__BRAZESYNC.cb_id.promo_banner__"));
        assert!(r.new_body.contains("${promo_banner}"));
        assert_eq!(r.cb_id_rewrites, 1);
    }

    #[test]
    fn dedupes_duplicate_url_with_sequential_suffix() {
        let body = r#"
<a href="https://example.com/cta">{{x | lid: 'ai8kexrxcp03'}}A</a>
<a href="https://example.com/cta">{{x | lid: 'bj9lfsysxq14'}}B</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("__BRAZESYNC.lid.cta__"));
        assert!(r.new_body.contains("__BRAZESYNC.lid.cta_2__"));
    }

    #[test]
    fn plaintext_url_anchor_works() {
        let body = "Click https://example.com/promo {{x | lid: 'ai8kexrxcp03'}} now.";
        let r = templatize_body(body, FieldKind::EmailPlainBody);
        assert!(r.new_body.contains("__BRAZESYNC.lid.promo__"));
    }

    #[test]
    fn repeated_cb_id_name_reuses_key() {
        let body = "{{content_blocks.${promo} | id: 'cb10'}} ... \
                    {{content_blocks.${promo} | id: 'cb10'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        let occurrences = r.new_body.matches("__BRAZESYNC.cb_id.promo__").count();
        assert_eq!(occurrences, 2);
    }

    #[test]
    fn lid_inside_href_attribute_value_uses_enclosing_anchor() {
        let body = r#"<a href="https://med.example.com/product/jaypirca/50mg/?lid={{${cblid} | lid: 'ai8kexrxcp03'}}"><img src="x"/></a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("__BRAZESYNC.lid.link_50mg__"));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn vml_href_anchors_lid() {
        let body = r#"<v:roundrect xmlns:v="urn:schemas-microsoft-com:vml" href="https://hokto.example.com/page/?lid={{${cblid} | lid: 'ulab324mjv2a'}}" style="…"></v:roundrect>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("__BRAZESYNC.lid.page__"));
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn data_prefixed_attrs_are_not_treated_as_url_anchor() {
        let body = r#"<button data-action="track" data-href="ignored">{{x | lid: 'ulab324mjv2a'}}</button>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        // Falls back to sequential `link` key, no URL anchor.
        assert!(r.new_body.contains("__BRAZESYNC.lid.link__"));
        assert!(!r.warnings.is_empty());
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
