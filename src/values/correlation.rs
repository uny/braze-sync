//! Remote-body correlation primitives.
//!
//! Extract lid / cb_id values from a remote body together with the
//! anchor used to pair them with template placeholders.
//!
//! - HTML lid: anchor = the URL attribute of the enclosing element.
//!   Same-URL occurrences are matched by appearance order.
//! - Plaintext lid: anchor = the raw `https?://…` run at or before the
//!   lid. The run spans whole Liquid tags, so a URL built from Liquid
//!   can enclose the lid rather than merely precede it.
//! - cb_id: anchor = `${NAME}` in the same Liquid include.

use regex_lite::Regex;
use std::sync::OnceLock;

use crate::values::placeholder::TOKEN;

/// Shared pattern for raw lid values so templatize and correlation cannot drift.
pub(crate) const LID_VALUE_PATTERN: &str = "[a-z0-9][a-z0-9_]*";

/// Stand-in for a Braze-managed filter inside an anchor key. Contains no
/// `?` / `#` so it survives the query/fragment cut below.
const MANAGED_FILTER_MASK: &str = "|<braze-managed>";

/// Normalize a URL for anchor comparison: mask Braze-managed filters,
/// then keep `scheme://host/path` and drop `?query` / `#fragment`.
///
/// Both sides of a correlation run through this, so the key must never
/// embed the very value being correlated. Cutting at the first literal
/// `?` is not enough: when the query separator is Liquid-templated
/// (`{{ item.url }}{{ sep }}lid={{ x | lid: '…' }}`) there is no literal
/// `?`, so the lid filter would stay in the key — `__BRAZESYNC__` on the
/// template side, the live value on the remote side, never equal. Masking
/// the filter first makes both sides collapse to the same string.
///
/// Callers pass already-detected URLs, but the rewrite is idempotent and
/// shape-driven rather than scheme-driven, so it is safe to apply in
/// either direction. Note that it does *not* pass non-URL text through
/// untouched: anything after a `?` / `#` is dropped and any managed
/// filter is masked, whatever the input looks like.
pub fn normalize_url(url: &str) -> String {
    let masked = lid_filter_re().replace_all(url, MANAGED_FILTER_MASK);
    // `${1}` keeps the `{{content_blocks.${NAME}` prefix the cb_id regex
    // had to capture; only the `| id: '…'` after it is masked.
    let cb_replacement = format!("${{1}}{MANAGED_FILTER_MASK}");
    let masked = cb_id_filter_re().replace_all(masked.as_ref(), cb_replacement.as_str());
    let stop = masked.find(['?', '#']).unwrap_or(masked.len());
    masked[..stop].to_string()
}

fn lid_filter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `| lid: 'X'` in either its live form or its templatized
        // `__BRAZESYNC__` form. `lid` is Braze's own filter, so matching it
        // by name anywhere is safe.
        let lid = format!("(?:{LID_VALUE_PATTERN}|{TOKEN})");
        Regex::new(&format!(r#"\|\s*lid:\s*(?:"{lid}"|'{lid}')"#))
            .expect("lid filter regex is valid")
    })
}

fn cb_id_filter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // `id` is NOT a Braze-reserved filter name — a template may define
        // its own. Only the `| id: 'cbN'` inside a `content_blocks.${NAME}`
        // include is Braze-managed, so anchor on that prefix (mirroring
        // `cb_id_include_re`) and keep it in the key. Masking every
        // `| id: 'cbN'` would collapse two anchors that differ only in an
        // unrelated `id` filter, and `resolve_lid_batch`'s FIFO would then
        // hand each link the other's live lid.
        let cb = format!("(?:cb[0-9]+|{TOKEN})");
        Regex::new(&format!(
            r#"(\{{\{{\s*content_blocks\.\$\{{\s*[^\s}}|]+\s*\}}\s*)\|\s*id:\s*(?:"{cb}"|'{cb}')"#
        ))
        .expect("cb_id filter regex is valid")
    })
}

fn href_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Tolerant of attribute order and either quote style. Matches
        // `href`, `src`, `action` — with an optional namespace prefix
        // like `xlink:` or `v:` — on any element, not just `<a>`. This
        // mirrors templatize::url_attr_re so VML / SVG CTAs whose lid
        // sits inside a non-anchor element's href round-trip through
        // apply/diff resolution. Leading `\s` (not `\b`) prevents
        // `data-href`-style custom attributes from tail-matching.
        Regex::new(
            r#"(?i)<[a-z][a-z0-9_.:-]*\b[^>]*?\s(?:[a-z][a-z0-9_-]*:)?(?:href|src|action)\s*=\s*(?:"([^"]*)"|'([^']*)')"#,
        )
        .expect("href regex is valid")
    })
}

fn lid_value_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            r#"\|\s*lid:\s*(?:"({p})"|'({p})')"#,
            p = LID_VALUE_PATTERN
        ))
        .expect("lid value regex is valid")
    })
}

fn plaintext_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Greedy `[^\s<>"']` runs up to whitespace or a quote/angle —
        // good enough for Braze plaintext where URLs aren't routinely
        // wrapped in markup. Trailing punctuation is trimmed post-hoc
        // (see `trim_trailing_punctuation`).
        //
        // A Liquid output tag is one atom, so a URL assembled from Liquid
        // keeps its whole run. Without it the run ends *inside* the tag —
        // at the space or the `'` — and two failures follow, because
        // whether a byte is whitespace there depends on formatting that
        // `templatize` rewrites:
        //
        //   1. `templatize` canonicalizes `{{x|lid:'v'}}` to
        //      `{{x| lid: '__BRAZESYNC__'}}`, inserting a space. The
        //      template run then stops at that space (`…{{x|`) while the
        //      remote run stops at the quote (`…{{x|lid`), so the keys
        //      never match and every apply overwrites the live lid with a
        //      fallback slug.
        //   2. Anything past the tag is dropped, so `…{{x | lid: 'a'}}/one`
        //      and `…{{x | lid: 'b'}}/two` collapse to one key and
        //      `resolve_lid_batch`'s FIFO hands each link the other's lid.
        //
        // Spanning the tag also lets `normalize_url` mask the filter,
        // which is what makes the key formatting-insensitive. The
        // alternative only fires on a *closed* `{{…}}`: a stray `{`
        // falls through to the character class, which admits it anyway.
        Regex::new(r#"https?://(?:\{\{[^{}]*\}\}|[^\s<>"'])+"#)
            .expect("plaintext URL regex is valid")
    })
}

fn cb_id_include_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Captures `${NAME}` (group 1) and `cbN` (group 2) from
        //   {{content_blocks.${NAME} | id: 'cbN'}}
        // Matches existing dependency-graph regex in
        // src/diff/content_block_order.rs but tightened to require
        // `| id: '…'` form (we need the cbN value, not just NAME).
        Regex::new(
            r#"\{\{\s*content_blocks\.\$\{\s*([^\s}|]+)\s*\}\s*\|\s*id:\s*(?:"(cb[0-9]+)"|'(cb[0-9]+)')\s*\}\}"#,
        )
        .expect("cb_id include regex is valid")
    })
}

/// Trim trailing punctuation that a greedy URL match would otherwise
/// swallow. The following are *always* trimmed:
/// `.`, `,`, `;`, `:`, `!`, `?`, `>`. The closers `)` and `]` are
/// trimmed *only* when the URL is preceded by the corresponding opener
/// (`(` or `[`) — Markdown-style `[text](https://…)` is the motivating
/// case. This conservative rule preserves URLs that legitimately end
/// in `)`/`]` (e.g., Wikipedia disambiguation pages) when no opener is
/// present in the surrounding text.
fn trim_trailing_punctuation(url: &str, preceded_by: Option<char>) -> &str {
    let pair_closer = match preceded_by {
        Some('(') => Some(')'),
        Some('[') => Some(']'),
        Some('<') => Some('>'),
        _ => None,
    };
    let mut end = url.len();
    while end > 0 {
        let c = url[..end].chars().last().unwrap();
        let drop_general = matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '>');
        let drop_pair = Some(c) == pair_closer;
        if drop_general || drop_pair {
            end -= c.len_utf8();
        } else {
            break;
        }
    }
    &url[..end]
}

/// One remote-side correlation point: a URL anchor (in field byte
/// offset order) paired with the lid value that follows it in the
/// same anchor scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LidCorrelation {
    /// Normalized URL anchor.
    pub url: String,
    /// The lid value extracted from `| lid: '…'`.
    pub value: String,
    /// Byte offset where the `<a href>` (HTML) or raw URL (plaintext)
    /// begins. Useful for ordering and ambiguity reporting.
    pub url_offset: usize,
}

/// Extract `(url, lid_value)` pairs from an HTML field by pairing each
/// `<a href="…">` with the next `| lid: '…'` that follows it before
/// the next `<a href>` or end of string. Unpaired anchors are skipped.
pub fn extract_html_lid_values(body: &str) -> Vec<LidCorrelation> {
    pair_urls_with_lids(href_iter(body), body)
}

/// Extract `(url, lid_value)` pairs from a plaintext field. Same
/// pairing rule as HTML but URLs come from raw `https?://…` matches.
pub fn extract_plaintext_lid_values(body: &str) -> Vec<LidCorrelation> {
    pair_urls_with_lids(plaintext_url_anchors(body), body)
}

/// Extract raw lid values in field appearance order without any URL
/// anchoring. Used for subject / preheader where no anchor exists; the
/// caller matches template placeholders to remote values positionally.
pub fn extract_lid_values_unanchored(body: &str) -> Vec<String> {
    lid_value_re()
        .captures_iter(body)
        .filter_map(|c| c.get(1).or(c.get(2)).map(|m| m.as_str().to_string()))
        .collect()
}

fn href_iter(body: &str) -> Vec<(usize, String)> {
    href_re()
        .captures_iter(body)
        .filter_map(|cap| {
            let whole = cap.get(0)?;
            let url = cap
                .get(1)
                .or(cap.get(2))
                .map(|m| m.as_str())
                .unwrap_or_default();
            Some((whole.start(), normalize_url(url)))
        })
        .collect()
}

/// Scan `body` for plaintext URLs, returning `(byte offset, anchor key)`
/// in appearance order.
///
/// Shared by remote-side extraction and template-side anchor lookup:
/// trailing-punctuation trimming and [`normalize_url`] must be applied to
/// both, or a URL like `https://x.com/end.` keys differently on each side
/// and the correlation can never match.
///
/// Note that [`normalize_url`]'s masking is inert here: `plaintext_url_re`
/// stops at `'` / `"`, so a quoted managed filter is never inside the
/// match and the key ends mid-filter (`…{{x|lid`). Both sides truncate at
/// the same byte, so they still agree — but the key therefore also drops
/// everything after the filter, and two plaintext URLs differing only in
/// that tail share one anchor. `resolve_lid_batch` warns when a bucket
/// holds more than one remote value.
pub(crate) fn plaintext_url_anchors(body: &str) -> Vec<(usize, String)> {
    plaintext_url_re()
        .find_iter(body)
        .map(|m| {
            let raw = m.as_str();
            let preceded_by = if m.start() > 0 {
                body[..m.start()].chars().last()
            } else {
                None
            };
            let trimmed = trim_trailing_punctuation(raw, preceded_by);
            (m.start(), normalize_url(trimmed))
        })
        .collect()
}

fn pair_urls_with_lids(urls: Vec<(usize, String)>, body: &str) -> Vec<LidCorrelation> {
    let lids: Vec<(usize, String)> = lid_value_re()
        .captures_iter(body)
        .filter_map(|cap| {
            let whole = cap.get(0)?;
            let value = cap.get(1).or(cap.get(2)).map(|m| m.as_str().to_string())?;
            Some((whole.start(), value))
        })
        .collect();

    let mut out = Vec::new();
    for (i, (url_off, url)) in urls.iter().enumerate() {
        let next_url_off = urls.get(i + 1).map(|(o, _)| *o).unwrap_or(body.len());
        for (_, value) in lids
            .iter()
            .filter(|(off, _)| *off > *url_off && *off < next_url_off)
        {
            out.push(LidCorrelation {
                url: url.clone(),
                value: value.clone(),
                url_offset: *url_off,
            });
        }
    }
    out
}

/// One cb_id include occurrence extracted from a remote body. Slug is
/// the key derived from `${NAME}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CbIdCorrelation {
    /// The verbatim `${NAME}` content_block name from the include.
    pub name: String,
    /// `cbN` form, e.g. `cb42`.
    pub value: String,
    /// Slug-form key.
    pub key: String,
}

/// Extract every `{{content_blocks.${NAME} | id: 'cbN'}}` from `body`.
pub fn extract_cb_id_values(body: &str) -> Vec<CbIdCorrelation> {
    cb_id_include_re()
        .captures_iter(body)
        .filter_map(|cap| {
            let name = cap.get(1)?.as_str().to_string();
            let value = cap.get(2).or(cap.get(3)).map(|m| m.as_str().to_string())?;
            let key = slug_for_cb_id(&name);
            Some(CbIdCorrelation { name, value, key })
        })
        .collect()
}

/// Slug a content_block name for use as a `cb_id` key.
///
/// Keys never end in `_` — a trailing underscore followed by the `__`
/// envelope close produces ambiguous triple-underscores in templates.
pub fn slug_for_cb_id(name: &str) -> String {
    let base = slug_core(name);
    if base.is_empty() {
        "cb".to_string()
    } else if base.starts_with(|c: char| c.is_ascii_digit()) {
        format!("cb_{base}")
    } else {
        base
    }
}

/// Slug a URL path tail or arbitrary anchor for use as a `lid` key.
/// `link` prefix is applied when the source produces no meaningful
/// ASCII content. Keys never end in `_` — see
/// [`slug_for_cb_id`] for the rationale.
pub fn slug_for_lid(source: &str) -> String {
    let base = slug_core(source);
    if base.is_empty() {
        "link".to_string()
    } else if base.starts_with(|c: char| c.is_ascii_digit()) {
        format!("link_{base}")
    } else {
        base
    }
}

fn slug_core(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_underscore = false;
    for ch in s.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if last_underscore {
                continue;
            }
            last_underscore = true;
        } else {
            last_underscore = false;
        }
        out.push(mapped);
    }
    let trimmed = out.trim_matches('_');
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_query_and_fragment() {
        assert_eq!(
            normalize_url("https://example.com/x?utm=1"),
            "https://example.com/x"
        );
        assert_eq!(
            normalize_url("https://example.com/x#frag"),
            "https://example.com/x"
        );
        assert_eq!(
            normalize_url("https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn normalize_masks_managed_filters_so_both_sides_agree() {
        // No literal `?`, so the cut alone leaves the lid value in the
        // key. Template and remote must still normalize identically.
        let template = "{{ item.url }}{{ sep }}lid={{ x | lid: '__BRAZESYNC__' }}";
        let remote = "{{ item.url }}{{ sep }}lid={{ x | lid: 'liveeeeeeee1' }}";
        assert_eq!(normalize_url(template), normalize_url(remote));
        assert!(!normalize_url(template).contains("__BRAZESYNC__"));

        let tmpl_cb = "{{content_blocks.${base} | id: '__BRAZESYNC__'}}/go";
        let remote_cb = "{{content_blocks.${base} | id: 'cb42'}}/go";
        assert_eq!(normalize_url(tmpl_cb), normalize_url(remote_cb));
    }

    #[test]
    fn normalize_leaves_unrelated_filters_intact() {
        // Only the value shapes templatize round-trips are masked, so an
        // unrelated `id`-named filter still distinguishes two anchors.
        assert_eq!(
            normalize_url("{{ a | id: 'not-a-cb-id' }}"),
            "{{ a | id: 'not-a-cb-id' }}"
        );
        assert_ne!(
            normalize_url("{{ a | upcase }}"),
            normalize_url("{{ b | upcase }}")
        );
    }

    #[test]
    fn normalize_keeps_unrelated_id_filter_with_cb_shaped_value_distinct() {
        // `id` is not reserved: a template may define its own filter named
        // `id` whose value happens to look like a cb_id. Masking by name +
        // value shape alone would collapse these two anchors into one FIFO
        // bucket, and a dashboard-side link reorder would then swap the two
        // links' live lid values — permanently, since both stay valid.
        assert_ne!(
            normalize_url("https://x/{{ product | id: 'cb1' }}"),
            normalize_url("https://x/{{ product | id: 'cb2' }}")
        );
        // The genuine managed form — inside a `content_blocks.${NAME}`
        // include — must still collapse across template/remote.
        assert_eq!(
            normalize_url("{{content_blocks.${base} | id: '__BRAZESYNC__'}}/go"),
            normalize_url("{{content_blocks.${base} | id: 'cb42'}}/go")
        );
        // ...while still distinguishing two different includes.
        assert_ne!(
            normalize_url("{{content_blocks.${a} | id: 'cb1'}}"),
            normalize_url("{{content_blocks.${b} | id: 'cb2'}}")
        );
    }

    #[test]
    fn html_lid_pairs_each_anchor_with_following_value() {
        let body = r#"<p>
<a href="https://example.com/a">{{ x | lid: 'lidvalueaa1' }}A</a>
<a href="https://example.com/b">{{ x | lid: 'lidvaluebb2' }}B</a>
</p>"#;
        let pairs = extract_html_lid_values(body);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].url, "https://example.com/a");
        assert_eq!(pairs[0].value, "lidvalueaa1");
        assert_eq!(pairs[1].url, "https://example.com/b");
        assert_eq!(pairs[1].value, "lidvaluebb2");
    }

    #[test]
    fn html_lid_unpaired_anchor_is_skipped() {
        let body = r#"<a href="https://example.com/a">no lid here</a>
<a href="https://example.com/b">{{ x | lid: 'lidvaluebb2' }}B</a>"#;
        let pairs = extract_html_lid_values(body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].url, "https://example.com/b");
    }

    #[test]
    fn html_lid_handles_both_quote_styles_and_query_string() {
        let body = r#"<a href='https://example.com/x?utm=foo'>{{ x | lid: "lidvaluexyz1" }}X</a>"#;
        let pairs = extract_html_lid_values(body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].url, "https://example.com/x");
        assert_eq!(pairs[0].value, "lidvaluexyz1");
    }

    #[test]
    fn plaintext_run_spans_liquid_tags_and_keys_past_them() {
        let anchors = plaintext_url_anchors(
            "Go https://x.com/p{{sep}}lid={{x | lid: 'aaaaaaaaaaa'}}/alpha and \
             https://x.com/p{{sep}}lid={{x|lid:'bbbbbbbbbbb'}}/beta now",
        );
        let keys: Vec<&str> = anchors.iter().map(|(_, u)| u.as_str()).collect();
        // The run covers the whole tag, so the suffix past it survives and
        // the two links stay distinguishable; the filter itself is masked,
        // so the compact and spaced spellings agree.
        assert_eq!(
            keys,
            vec![
                "https://x.com/p{{sep}}lid={{x |<braze-managed>}}/alpha",
                "https://x.com/p{{sep}}lid={{x|<braze-managed>}}/beta",
            ]
        );
    }

    #[test]
    fn plaintext_run_stops_at_whitespace_outside_a_liquid_tag() {
        // The documented plaintext shape puts the lid tag *after* the URL.
        // Spanning a `{{…}}` must not make the run jump the space into it,
        // or the anchor stops being the URL. Same for a bare `| lid:` in
        // prose (`plaintext_lid_trims_trailing_punctuation` covers pairing
        // for that one) and for quoted prose around a URL.
        for (body, want) in [
            (
                "Click https://example.com/promo {{x | lid: 'lidplain01a'}} now.",
                "https://example.com/promo",
            ),
            (
                "Visit (https://example.com/cta) | lid: 'lidplain01a' for the deal.",
                "https://example.com/cta",
            ),
            (
                r#"He said "visit https://x.com/a" then "bye""#,
                "https://x.com/a",
            ),
        ] {
            let anchors = plaintext_url_anchors(body);
            assert_eq!(anchors.len(), 1, "{body}");
            assert_eq!(anchors[0].1, want, "{body}");
        }
    }

    #[test]
    fn plaintext_lid_trims_trailing_punctuation() {
        // Markdown-style link: closing `)` must be trimmed because the
        // URL was preceded by `(`. Following `| lid:` syntax in
        // plaintext is unusual but Braze does emit it.
        let body = "Visit (https://example.com/cta) | lid: 'lidplain01a' for the deal.";
        let pairs = extract_plaintext_lid_values(body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].url, "https://example.com/cta");
        assert_eq!(pairs[0].value, "lidplain01a");
    }

    #[test]
    fn plaintext_lid_trims_sentence_period() {
        let body = "See https://example.com/end. | lid: 'lidplain02b'";
        let pairs = extract_plaintext_lid_values(body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].url, "https://example.com/end");
    }

    #[test]
    fn cb_id_extracts_name_and_value() {
        // Liquid variable names inside `${...}` carry no whitespace by
        // construction — matches the dep-graph regex in
        // src/diff/content_block_order.rs.
        let body = "before {{content_blocks.${promo_banner} | id: 'cb42'}} after";
        let pairs = extract_cb_id_values(body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].name, "promo_banner");
        assert_eq!(pairs[0].value, "cb42");
        assert_eq!(pairs[0].key, "promo_banner");
    }

    #[test]
    fn cb_id_handles_multiple_includes() {
        let body = "{{content_blocks.${alpha} | id: 'cb1'}} {{content_blocks.${beta} | id: 'cb2'}}";
        let pairs = extract_cb_id_values(body);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].name, "alpha");
        assert_eq!(pairs[0].value, "cb1");
        assert_eq!(pairs[0].key, "alpha");
        assert_eq!(pairs[1].name, "beta");
        assert_eq!(pairs[1].value, "cb2");
    }

    #[test]
    fn cb_id_slug_uses_cb_prefix_for_empty_or_digit_start() {
        assert_eq!(slug_for_cb_id("2024_summer"), "cb_2024_summer");
        assert_eq!(slug_for_cb_id(""), "cb");
        assert_eq!(slug_for_cb_id("My Promo Banner"), "my_promo_banner");
        assert_eq!(slug_for_cb_id("cb_promo_image"), "cb_promo_image");
    }

    #[test]
    fn lid_slug_uses_link_prefix_for_empty_or_digit_start() {
        assert_eq!(slug_for_lid("/spring-sale"), "spring_sale");
        assert_eq!(slug_for_lid("/"), "link");
        assert_eq!(slug_for_lid("123"), "link_123");
        // Non-ASCII source collapses to empty Unicode rule.
        assert_eq!(slug_for_lid("プロモ"), "link");
    }

    #[test]
    fn slug_collapses_multiple_separators() {
        assert_eq!(slug_for_lid("foo//bar--baz"), "foo_bar_baz");
        assert_eq!(slug_for_lid("--leading"), "leading");
    }

    #[test]
    fn lid_value_extracts_short_fallback_slug() {
        let vals = extract_lid_values_unanchored("{{ x | lid: 'cta' }}");
        assert_eq!(vals, vec!["cta"]);
    }

    #[test]
    fn lid_value_extracts_underscore_fallback_slug() {
        let vals =
            extract_lid_values_unanchored("{{ x | lid: 'lid_1' }} {{ y | lid: 'spring_sale' }}");
        assert_eq!(vals, vec!["lid_1", "spring_sale"]);
    }

    #[test]
    fn html_lid_pairs_fallback_slug_with_anchor() {
        let body = r#"<a href="https://example.com/promo">{{ x | lid: 'cta' }}Buy</a>"#;
        let pairs = extract_html_lid_values(body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].value, "cta");
        assert_eq!(pairs[0].url, "https://example.com/promo");
    }

    #[test]
    fn lid_value_ignores_brazesync_placeholder() {
        let vals = extract_lid_values_unanchored("{{ x | lid: '__BRAZESYNC__' }}");
        assert!(vals.is_empty());
    }

    #[test]
    fn lid_value_extracts_digit_leading_value() {
        let vals = extract_lid_values_unanchored("{{ x | lid: '275ua26snuk7' }}");
        assert_eq!(vals, vec!["275ua26snuk7"]);
    }

    #[test]
    fn html_lid_pairs_digit_leading_value() {
        let body = r#"<a href="https://example.com/sale">{{ x | lid: '47043wg2o5wi' }}Buy</a>"#;
        let pairs = extract_html_lid_values(body);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].value, "47043wg2o5wi");
    }
}
