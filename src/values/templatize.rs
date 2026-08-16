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
        // `trim_matches`/`is_cb_id_whitespace` here, not `str::trim_ascii` /
        // `u8::is_ascii_whitespace` — the latter excludes U+000B (vertical
        // tab), while the narrow `[^\s}|]+` regexes this name is later
        // checked against (`braze_managed::cb_id_template_re` et al.) use
        // regex `\s`, which includes it. A name differing only in a `\v`
        // must agree on both sides, or a name this pass treats as clean
        // can still fail to resolve at apply time via those regexes.
        let trimmed = name.trim_matches(is_cb_id_whitespace);
        let has_internal_whitespace = trimmed.chars().any(is_cb_id_whitespace);
        if trimmed.is_empty() || has_internal_whitespace {
            // Braze content block names cannot be empty or contain
            // whitespace, so this include can never denote a real content
            // block. Leave it raw rather than manage it — but say so,
            // instead of the silent gap #85 used to be
            // (`correlation::cb_id_filter_re` still masks the raw `cbN`
            // out of any nearby anchor key, so this does not corrupt an
            // unrelated lid the way it used to).
            let problem = if trimmed.is_empty() {
                "is empty"
            } else {
                "contains whitespace"
            };
            // Escaped for display: the capture is now broad enough to
            // admit control bytes (e.g. a stray newline), and this string
            // is later written verbatim to the terminal by the CLI.
            let shown = escape_for_warning(trimmed);
            warnings.push(format!(
                "cb_id include at byte {}: content block name `{shown}` {problem}, \
                 which Braze does not allow — leaving this include untemplated; \
                 fix the name or the reference",
                whole.start()
            ));
            continue;
        }
        spans.push(DetectionSpan {
            range: whole.range(),
            replacement: format!("{{{{content_blocks.${{{trimmed}}} | id: '__BRAZESYNC__'}}}}"),
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

/// Whitespace per regex_lite's ASCII `\s` class (`[\t\n\v\f\r ]`) — not
/// `char::is_ascii_whitespace`, which excludes U+000B vertical tab. This
/// is the definition the narrow `[^\s}|]+` cb_id regexes use, so a name's
/// validity here must agree with what those regexes will later match.
fn is_cb_id_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t'..='\r')
}

/// Escape control bytes for safe interpolation into a warning string that
/// the CLI writes verbatim to the terminal. The name capture is broad
/// enough to admit a stray newline or escape byte; this keeps one from
/// injecting a line into the operator's summary output.
fn escape_for_warning(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() {
                format!("\\u{{{:x}}}", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect()
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
        // `${NAME}` is captured broadly (`[^}|]*`, not `[^\s}|]+`) so an
        // include whose name holds whitespace is still detected. Braze
        // forbids whitespace in a real content block name, so such an
        // include can never be managed — but it must be *seen* here to
        // warn about it (#85) rather than silently skipped, which is what
        // let the raw `cbN` sit unmanaged with no diagnostic at all.
        Regex::new(
            r#"\{\{\s*content_blocks\.\$\{([^}|]*)\}\s*\|\s*id:\s*(?:"cb[0-9]+"|'cb[0-9]+')\s*\}\}"#,
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
    fn cb_id_include_with_whitespace_in_name_is_left_untemplated_with_warning() {
        // #85: Braze forbids whitespace in a content block name, so this
        // include can never denote a real one. It must not be silently
        // skipped — that was the bug.
        let body = "{{content_blocks.${Plan (US)} | id: 'cb1'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.cb_id_rewrites, 0);
        assert_eq!(r.new_body, body, "left untouched");
        assert!(
            r.warnings.iter().any(|w| w.contains("whitespace")),
            "got: {:?}",
            r.warnings
        );
    }

    #[test]
    fn cb_id_include_with_vertical_tab_in_name_is_left_untemplated_with_warning() {
        // regex_lite's `\s` (used by the still-narrow `[^\s}|]+` regexes
        // in braze_managed.rs) includes U+000B vertical tab, but Rust's
        // `is_ascii_whitespace`/`trim_ascii` do not. A name that is
        // "clean" only by the latter definition would be managed here
        // with no warning, then permanently fail to resolve at apply
        // time via those still-narrow regexes.
        let body = "{{content_blocks.${Plan\u{000B}US} | id: 'cb1'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.cb_id_rewrites, 0, "must not silently manage it");
        assert_eq!(r.new_body, body, "left untouched");
        assert!(
            r.warnings.iter().any(|w| w.contains("whitespace")),
            "got: {:?}",
            r.warnings
        );
    }

    #[test]
    fn cb_id_include_with_blank_name_is_left_untemplated_with_warning() {
        let body = "{{content_blocks.${   } | id: 'cb1'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.cb_id_rewrites, 0);
        assert_eq!(r.new_body, body, "left untouched");
        assert!(
            r.warnings.iter().any(|w| w.contains("is empty")),
            "got: {:?}",
            r.warnings
        );
    }

    #[test]
    fn cb_id_warning_escapes_control_bytes_in_the_name() {
        // The name capture is broad enough to admit a raw newline; the
        // warning is later written verbatim to the terminal by the CLI
        // (`eprintln!`), so a literal `\n` here must not inject a line.
        let body = "{{content_blocks.${evil\nfake: all clear} | id: 'cb1'}}";
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert_eq!(r.cb_id_rewrites, 0);
        assert!(
            r.warnings.iter().all(|w| !w.contains('\n')),
            "a literal newline must not reach the warning text: {:?}",
            r.warnings
        );
        assert!(
            r.warnings.iter().any(|w| w.contains("\\u{a}")),
            "the newline must be visibly escaped: {:?}",
            r.warnings
        );
    }

    #[test]
    fn cb_id_include_name_padding_is_trimmed() {
        let body = "{{content_blocks.${ promo_banner } | id: 'cb42'}}";
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
        let body = r#"<a href="https://example.com/sale">{{x | lid: '275ua26snuk7'}}</a>"#;
        let r = templatize_body(body, FieldKind::ContentBlock);
        assert!(
            r.new_body.contains("| lid: '__BRAZESYNC__'"),
            "got: {}",
            r.new_body
        );
        assert_eq!(r.lid_rewrites, 1);
    }
}
