//! Migration pass: raw-lid / raw-cb_id bodies → templated bodies.
//!
//! Powers `braze-sync templatize`. All functions are pure — they take
//! a body string + field kind and return the rewritten body plus any
//! warnings the operator should see.
//!
//! v0.16 model: raw `| lid: 'X'` and `{{content_blocks.${NAME} | id: 'cbN'}}`
//! are rewritten to the anonymous token `__BRAZESYNC__`. The raw values
//! are NOT persisted — they are re-fetched from the remote body at
//! apply/diff time (see [`crate::values::braze_managed`]).

use crate::values::correlation::LID_VALUE_PATTERN;
use regex_lite::Regex;
use std::sync::OnceLock;

/// Which Liquid context the body belongs to.
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
    /// where the resolver falls back to positional FIFO).
    pub warnings: Vec<String>,
}

/// Rewrite every raw `| lid: 'X'` and `{{content_blocks.${NAME} | id: 'cbN'}}`
/// to the anonymous `__BRAZESYNC__` token. Idempotent: the detection
/// regexes require raw literals ([`LID_VALUE_PATTERN`] for lid, `cb[0-9]+` for
/// cb_id), so an already-templated `__BRAZESYNC__` never re-matches.
pub fn templatize_body(body: &str, field: FieldKind) -> TemplatizedField {
    let mut spans: Vec<DetectionSpan> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut lid_rewrites = 0usize;
    let mut cb_id_rewrites = 0usize;

    for m in lid_match_re().captures_iter(body) {
        let whole = m.get(0).expect("group 0 always present");
        if matches!(field, FieldKind::EmailSubject | FieldKind::EmailPreheader) {
            warnings.push(format!(
                "lid detected in subject/preheader at byte {}; resolved \
                 positionally at apply/diff time (Nth placeholder = Nth remote lid). \
                 Verify rendered output if the field contains multiple lid values.",
                whole.start()
            ));
        }
        spans.push(DetectionSpan {
            range: whole.range(),
            replacement: "| lid: '__BRAZESYNC__'".to_string(),
        });
        lid_rewrites += 1;
    }

    for m in cb_id_match_re().captures_iter(body) {
        let whole = m.get(0).expect("group 0 always present");
        let name = m.get(1).expect("name capture present").as_str();
        spans.push(DetectionSpan {
            range: whole.range(),
            replacement: format!("{{{{content_blocks.${{{name}}} | id: '__BRAZESYNC__'}}}}"),
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
        Regex::new(&format!(
            r#"\|\s*lid:\s*(?:"{p}"|'{p}')"#,
            p = LID_VALUE_PATTERN
        ))
        .expect("lid match regex is valid")
    })
}

fn cb_id_match_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"\{\{\s*content_blocks\.\$\{\s*([^\s}|]+)\s*\}\s*\|\s*id:\s*(?:"cb[0-9]+"|'cb[0-9]+')\s*\}\}"#,
        )
        .expect("cb_id match regex is valid")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotent_on_already_templatized_body() {
        let body = "<p>__BRAZESYNC__ kept verbatim</p>";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.new_body, body);
        assert_eq!(r.lid_rewrites, 0);
    }

    #[test]
    fn idempotent_on_templatized_lid_filter() {
        let body = r#"<a href="https://example.com">{{x | lid: '__BRAZESYNC__'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.new_body, body);
        assert_eq!(r.lid_rewrites, 0);
    }

    #[test]
    fn rewrites_html_lid() {
        let body = r#"<a href="https://example.com/spring-sale">{{x | lid: 'ai8kexrxcp03'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(r.new_body.contains("| lid: '__BRAZESYNC__'"));
        assert_eq!(r.lid_rewrites, 1);
    }

    #[test]
    fn rewrites_cb_id_include() {
        let body = "{{content_blocks.${promo_banner} | id: 'cb42'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(
            r.new_body
                .contains("{{content_blocks.${promo_banner} | id: '__BRAZESYNC__'}}"),
            "got: {}",
            r.new_body
        );
        assert_eq!(r.cb_id_rewrites, 1);
    }

    #[test]
    fn multiple_lids_in_one_field_all_become_anonymous() {
        let body = r#"
<a href="https://example.com/cta">{{x | lid: 'ai8kexrxcp03'}}A</a>
<a href="https://example.com/cta">{{x | lid: 'bj9lfsysxq14'}}B</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        let n = r.new_body.matches("| lid: '__BRAZESYNC__'").count();
        assert_eq!(n, 2);
    }

    #[test]
    fn plaintext_url_lid_rewritten() {
        let body = "Click https://example.com/promo {{x | lid: 'ai8kexrxcp03'}} now.";
        let r = templatize_body(body, FieldKind::EmailPlainBody);
        assert!(r.new_body.contains("| lid: '__BRAZESYNC__'"));
    }

    #[test]
    fn repeated_cb_id_name_independent_replacements() {
        let body = "{{content_blocks.${promo} | id: 'cb10'}} ... \
                    {{content_blocks.${promo} | id: 'cb10'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        let n = r
            .new_body
            .matches("{{content_blocks.${promo} | id: '__BRAZESYNC__'}}")
            .count();
        assert_eq!(n, 2);
    }

    #[test]
    fn subject_lid_emits_positional_warning() {
        let body = "{{x | lid: 'ai8kexrxcp03'}}";
        let r = templatize_body(body, FieldKind::EmailSubject);
        assert!(r.new_body.contains("| lid: '__BRAZESYNC__'"));
        assert!(!r.warnings.is_empty());
    }

    #[test]
    fn rewrites_short_fallback_slug() {
        let body = r#"<a href="https://x.com/cta">{{x | lid: 'cta'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(
            r.new_body.contains("| lid: '__BRAZESYNC__'"),
            "got: {}",
            r.new_body
        );
        assert_eq!(r.lid_rewrites, 1);
    }

    #[test]
    fn rewrites_underscore_fallback_slug() {
        let body = r#"{{x | lid: 'lid_1'}} A {{y | lid: 'spring_sale'}}"#;
        let r = templatize_body(body, FieldKind::EmailSubject);
        let n = r.new_body.matches("| lid: '__BRAZESYNC__'").count();
        assert_eq!(n, 2, "got: {}", r.new_body);
    }

    #[test]
    fn rewrites_digit_leading_lid() {
        let body =
            r#"<a href="https://example.com/sale">{{x | lid: '275ua26snuk7'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(
            r.new_body.contains("| lid: '__BRAZESYNC__'"),
            "got: {}",
            r.new_body
        );
        assert_eq!(r.lid_rewrites, 1);
    }
}
