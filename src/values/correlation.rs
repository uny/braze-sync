//! Remote-body correlation primitives for `export` (RFC §2.5).
//!
//! These functions inspect a *remote* body (HTML, plaintext, subject,
//! preheader) and return the per-occurrence lid / cb_id values together
//! with the anchor used to correlate them back to the values file
//! entries.
//!
//! - HTML lid: anchor = the URL of the immediately-preceding
//!   `<a href="...">`. Multiple `<a>`s with the same URL fall back to
//!   appearance order (RFC §2.5 "Key 対応の曖昧性").
//! - Plaintext lid: anchor = the raw URL (`https?://…`) immediately
//!   preceding the `| lid: '…'` token; trailing punctuation is trimmed
//!   (RFC §5 Edge case for `]`/`)` etc.).
//! - subject / preheader lid: anchor = adjacent Liquid identifiers
//!   inside the same `{{…}}` block. Phase 3 first cut covers the URL
//!   variants; the anchor-only variant is supported by carrying the
//!   anchor string verbatim from the existing values entry.
//! - cb_id: anchor = the `${NAME}` inside the same Liquid token as
//!   `| id: 'cbN'`. NAME is the source for the slug-derived key.

use regex_lite::Regex;
use std::sync::OnceLock;

/// Normalize a URL for anchor comparison per RFC §2.2:
/// keep `scheme://host/path`, drop `?query` and `#fragment`.
///
/// Returns the input unchanged if it doesn't look like a URL with a
/// scheme — callers pass already-detected URLs, but normalizing
/// idempotently keeps the function safe to apply in either direction.
pub fn normalize_url(url: &str) -> String {
    let stop = url.find(['?', '#']).unwrap_or(url.len());
    url[..stop].to_string()
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
        // The pipe anchor (`|`) prevents false matches on hash literals
        // or unrelated keyword args that happen to spell `lid:`. Matches
        // both quote styles, and the value class matches the built-in
        // shape check (`^[a-z0-9]{8,}$`).
        Regex::new(r#"\|\s*lid:\s*(?:"([a-z0-9]{8,})"|'([a-z0-9]{8,})')"#)
            .expect("lid value regex is valid")
    })
}

fn plaintext_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Greedy `[^\s<>"]` runs up to whitespace or a quote/angle —
        // good enough for Braze plaintext where URLs aren't routinely
        // wrapped in markup. Trailing punctuation is trimmed post-hoc
        // (see `trim_trailing_punctuation`).
        Regex::new(r#"https?://[^\s<>"']+"#).expect("plaintext URL regex is valid")
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
/// swallow. Per RFC §5 Edge case, the following are *always* trimmed:
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
    pair_urls_with_lids(plaintext_url_iter(body), body)
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

fn plaintext_url_iter(body: &str) -> Vec<(usize, String)> {
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
        if let Some((_, value)) = lids
            .iter()
            .find(|(off, _)| *off > *url_off && *off < next_url_off)
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
/// the RFC §3 Q3 key derived from `${NAME}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CbIdCorrelation {
    /// The verbatim `${NAME}` content_block name from the include.
    pub name: String,
    /// `cbN` form, e.g. `cb42`.
    pub value: String,
    /// Slug-form key per RFC §3 Q3.
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

/// Slug a content_block name for use as a `cb_id` key per RFC §3 Q3.
///
/// Keys never end in `_`: when the input slugifies to empty the result
/// is the bare prefix (`cb`), not `cb_`. A trailing `_` followed by the
/// placeholder envelope `__` produces three consecutive underscores in
/// the rendered template, which is ambiguous to parse — the resolver
/// recovers it but operators tripped over the ambiguity (see CHANGELOG
/// for v0.14.3).
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
/// ASCII content (RFC §3 Q3). Keys never end in `_` — see
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
        // Non-ASCII source collapses to empty per RFC §3 Q3 Unicode rule.
        assert_eq!(slug_for_lid("プロモ"), "link");
    }

    #[test]
    fn slug_collapses_multiple_separators() {
        assert_eq!(slug_for_lid("foo//bar--baz"), "foo_bar_baz");
        assert_eq!(slug_for_lid("--leading"), "leading");
    }
}
