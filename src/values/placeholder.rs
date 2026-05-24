//! Placeholder extraction and resolution for `__BRAZESYNC.<type>.<key>__`.
//!
//! Syntax is fixed by RFC `feat-per-env-values.md` §2.3:
//!   - Double-underscore envelope
//!   - Dot namespace
//!   - `<type>` ∈ {`lid`, `cb_id`, `custom`, `global`}
//!   - `<key>` matches `^[a-z][a-z0-9_]*$`
//!
//! This module is intentionally *resource-shape-agnostic*: it returns the
//! `(type, key)` pairs and lets callers (Phase 2+ wiring) pick the right
//! namespace (resource-local vs global, field-scoped vs resource-scoped).

use regex_lite::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

/// Placeholder type. Matches RFC §2.3 enumeration exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaceholderType {
    Lid,
    CbId,
    Custom,
    Global,
}

impl PlaceholderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlaceholderType::Lid => "lid",
            PlaceholderType::CbId => "cb_id",
            PlaceholderType::Custom => "custom",
            PlaceholderType::Global => "global",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "lid" => Some(Self::Lid),
            "cb_id" => Some(Self::CbId),
            "custom" => Some(Self::Custom),
            "global" => Some(Self::Global),
            _ => None,
        }
    }
}

/// One placeholder occurrence within a body string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    pub ty: PlaceholderType,
    pub key: String,
    /// Byte offset where the literal `__BRAZESYNC.…__` token begins.
    pub start: usize,
    /// Byte offset (exclusive) where it ends.
    pub end: usize,
}

impl Placeholder {
    /// The textual form, useful for error messages: `__BRAZESYNC.lid.foo__`.
    pub fn literal(&self) -> String {
        format!("__BRAZESYNC.{}.{}__", self.ty.as_str(), self.key)
    }
}

const PREFIX: &str = "__BRAZESYNC.";
const CLOSE: &str = "__";

fn key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^[a-z][a-z0-9_]*$").expect("key regex is valid"))
}

/// Loose envelope-only regex per RFC §2.3 warning rule. Catches typos like
/// `__BRAZSYNC.…__` or unknown types like `__BRAZESYNC.url.foo__` so they
/// can be surfaced as warnings rather than silently passing through. The
/// inner classes deliberately allow `_` so typo-shaped placeholders whose
/// key contains an underscore (e.g. `spring_sale`) are still caught.
fn loose_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"__BRAZE?SYNC\.[A-Za-z0-9_]+\.[A-Za-z0-9_]+__")
            .expect("loose placeholder regex is valid")
    })
}

/// Extract every strict `__BRAZESYNC.<type>.<key>__` occurrence in `body`,
/// in order of appearance.
///
/// Parsing strategy: anchor on the literal `__BRAZESYNC.` prefix and the
/// *nearest* closing `__` (left-most), so a regex with greedy `[a-z0-9_]*`
/// can't merge two adjacent placeholders into one.
///
/// Legacy recovery: v0.14.2 and earlier emitted exactly two trailing-`_`
/// keys — `lid.link_` and `cb_id.cb_` (empty-slug fallbacks; see
/// [`slug_for_cb_id`] / [`slug_for_lid`]). The rendered envelope collapses
/// to e.g. `…link___` (key's trailing `_` + close `__`). Recovery is
/// scoped to those two exact `(type, key)` pairs so that hand-written
/// bodies like `__BRAZESYNC.custom.foo___bar` continue to parse as
/// key=`foo` + literal `_bar` (rather than silently mutating the key into
/// `foo_`). The double-`_` guard additionally keeps
/// `__BRAZESYNC.lid.link____bar__` parsed as key=`link` rather than
/// absorbing the adjacent `__bar__` token.
pub fn extract_placeholders(body: &str) -> Vec<Placeholder> {
    let mut out = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i + PREFIX.len() <= bytes.len() {
        let Some(rel) = body[i..].find(PREFIX) else {
            break;
        };
        let start = i + rel;
        let inner_start = start + PREFIX.len();
        let Some(rel_close) = body[inner_start..].find(CLOSE) else {
            break;
        };
        let close_start = inner_start + rel_close;
        let mut end = close_start + CLOSE.len();
        let inner = &body[inner_start..close_start];
        if let Some((ty_str, key)) = inner.split_once('.') {
            if let (Some(ty), true) = (PlaceholderType::parse(ty_str), key_re().is_match(key)) {
                let is_legacy_empty_slug = (ty == PlaceholderType::Lid && key == "link")
                    || (ty == PlaceholderType::CbId && key == "cb");
                let mut key = key.to_string();
                if is_legacy_empty_slug
                    && bytes.get(end) == Some(&b'_')
                    && bytes.get(end + 1) != Some(&b'_')
                {
                    key.push('_');
                    end += 1;
                }
                out.push(Placeholder {
                    ty,
                    key,
                    start,
                    end,
                });
                i = end;
                continue;
            }
        }
        // Not a valid strict placeholder; skip past the opening `__` so the
        // remainder is still scanned (and may be surfaced by the loose pass).
        i = start + CLOSE.len();
    }
    out
}

/// Find loose envelope matches that don't satisfy the strict pattern.
/// Caller surfaces these as warnings (RFC §2.3).
pub fn find_suspicious_placeholders(body: &str) -> Vec<String> {
    let strict_spans: Vec<(usize, usize)> = extract_placeholders(body)
        .into_iter()
        .map(|p| (p.start, p.end))
        .collect();
    loose_re()
        .find_iter(body)
        .filter(|m| {
            // Overlap (not equality) — the greedy loose regex can extend past
            // a valid envelope into adjacent `__...__` text. See regression
            // tests below.
            !strict_spans
                .iter()
                .any(|&(s, e)| m.start() < e && s < m.end())
        })
        .map(|m| m.as_str().to_string())
        .collect()
}

/// What the resolver couldn't satisfy. Aggregated by the pre-flight phase
/// (RFC §2.4 / §3 Q7) so apply abort can report every failure at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionError {
    UnknownKey {
        ty: PlaceholderType,
        key: String,
        start: usize,
    },
    /// Same `lid` key referenced more than once in a single body / field.
    /// RFC §5 edge case: lid is a per-click-context ID so re-use is
    /// conceptually wrong — abort rather than substitute the same value.
    /// `occurrences` holds the byte offsets of every reference so the
    /// failure report can point operators at the duplicates directly.
    DuplicateLidKey {
        key: String,
        occurrences: Vec<usize>,
    },
}

/// Flat key for the resolver's lookup table.
///
/// Phase 1 deliberately stays resource-shape-agnostic: callers supply a
/// flat `(type, key) -> value` map and the resolver doesn't know whether
/// it came from a resource-local namespace, a field-level namespace, or
/// the `globals.custom` scope. Phase 2+ wiring composes the table from
/// the right places per RFC §2.2.
pub type LookupKey = (PlaceholderType, String);

/// Resolve every placeholder in `body` against `lookup`. Returns the
/// resolved body on success, or every unresolved placeholder on failure
/// (errors are aggregated, never short-circuited — matches §3 Q7).
pub fn resolve_placeholders(
    body: &str,
    lookup: &BTreeMap<LookupKey, String>,
) -> Result<String, Vec<ResolutionError>> {
    let placeholders = extract_placeholders(body);
    let mut errors = Vec::new();

    let mut lid_occurrences: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for ph in &placeholders {
        if matches!(ph.ty, PlaceholderType::Lid) {
            lid_occurrences
                .entry(ph.key.clone())
                .or_default()
                .push(ph.start);
        }
    }
    for (key, occurrences) in lid_occurrences {
        if occurrences.len() > 1 {
            errors.push(ResolutionError::DuplicateLidKey { key, occurrences });
        }
    }

    for ph in &placeholders {
        let key: LookupKey = (ph.ty, ph.key.clone());
        if !lookup.contains_key(&key) {
            errors.push(ResolutionError::UnknownKey {
                ty: ph.ty,
                key: ph.key.clone(),
                start: ph.start,
            });
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // Substitute back-to-front so byte offsets remain stable across edits.
    let mut out = body.to_string();
    for ph in placeholders.iter().rev() {
        let key: LookupKey = (ph.ty, ph.key.clone());
        let value = lookup
            .get(&key)
            .expect("missing key would have been caught above");
        out.replace_range(ph.start..ph.end, value);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(pairs: &[(PlaceholderType, &str, &str)]) -> BTreeMap<LookupKey, String> {
        pairs
            .iter()
            .map(|(t, k, v)| ((*t, (*k).to_string()), (*v).to_string()))
            .collect()
    }

    #[test]
    fn extracts_strict_placeholders_in_order() {
        let body = "head __BRAZESYNC.lid.spring_sale__ mid __BRAZESYNC.cb_id.cb_hero__ tail";
        let found = extract_placeholders(body);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].ty, PlaceholderType::Lid);
        assert_eq!(found[0].key, "spring_sale");
        assert_eq!(found[1].ty, PlaceholderType::CbId);
        assert_eq!(found[1].key, "cb_hero");
        assert!(found[0].start < found[1].start);
    }

    #[test]
    fn rejects_unknown_type_in_strict_pass() {
        let body = "x __BRAZESYNC.url.foo__ y";
        assert!(extract_placeholders(body).is_empty());
    }

    #[test]
    fn rejects_uppercase_key_in_strict_pass() {
        let body = "x __BRAZESYNC.lid.Foo__ y";
        assert!(extract_placeholders(body).is_empty());
    }

    #[test]
    fn rejects_digit_leading_key_in_strict_pass() {
        let body = "x __BRAZESYNC.lid.1foo__ y";
        assert!(extract_placeholders(body).is_empty());
    }

    #[test]
    fn suspicious_picks_up_typos_and_unknown_types() {
        let body = "x __BRAZSYNC.lid.foo__ y __BRAZESYNC.url.bar__ z";
        let warns = find_suspicious_placeholders(body);
        assert_eq!(warns.len(), 2);
        assert!(warns.iter().any(|s| s.contains("BRAZSYNC")));
        assert!(warns.iter().any(|s| s.contains(".url.")));
    }

    #[test]
    fn suspicious_excludes_strict_matches() {
        let body = "__BRAZESYNC.lid.ok__";
        assert!(find_suspicious_placeholders(body).is_empty());
    }

    #[test]
    fn suspicious_ignores_trailing_double_underscore_text() {
        // Regression: greedy loose regex extends past a valid placeholder
        // into `__bold__`-style adjacent text and reports a span like
        // (0, 26) for `__BRAZESYNC.lid.foo__bar__`. That span overlaps the
        // real strict placeholder (0, 21), so it must not be surfaced.
        let body = "__BRAZESYNC.lid.foo__bar__";
        assert!(find_suspicious_placeholders(body).is_empty());
    }

    #[test]
    fn suspicious_ignores_adjacent_placeholders_sharing_underscores() {
        // Regression: with two adjacent strict placeholders joined by an
        // extra `__`, the loose regex finds a single match that overlaps
        // both strict spans. Overlap means "already covered" — no warning.
        let body = "__BRAZESYNC.lid.foo____BRAZESYNC.lid.bar__";
        assert!(find_suspicious_placeholders(body).is_empty());
    }

    #[test]
    fn resolves_when_all_keys_present() {
        let body = "before __BRAZESYNC.lid.cta__ middle __BRAZESYNC.custom.host__ end";
        let map = lookup(&[
            (PlaceholderType::Lid, "cta", "ai8kexrxcp03"),
            (PlaceholderType::Custom, "host", "api-prod.example.com"),
        ]);
        let resolved = resolve_placeholders(body, &map).unwrap();
        assert_eq!(
            resolved,
            "before ai8kexrxcp03 middle api-prod.example.com end"
        );
    }

    #[test]
    fn resolves_repeated_keys_to_same_value() {
        let body = "__BRAZESYNC.global.host__/a __BRAZESYNC.global.host__/b";
        let map = lookup(&[(PlaceholderType::Global, "host", "example.com")]);
        let resolved = resolve_placeholders(body, &map).unwrap();
        assert_eq!(resolved, "example.com/a example.com/b");
    }

    #[test]
    fn aggregates_unresolved_keys() {
        let body = "__BRAZESYNC.lid.a__ __BRAZESYNC.cb_id.b__ __BRAZESYNC.custom.c__";
        let map = lookup(&[(PlaceholderType::Lid, "a", "ai8kexrxcp03")]);
        let err = resolve_placeholders(body, &map).unwrap_err();
        assert_eq!(err.len(), 2);
        let keys: Vec<_> = err
            .iter()
            .map(|e| match e {
                ResolutionError::UnknownKey { ty, key, .. } => (*ty, key.clone()),
                ResolutionError::DuplicateLidKey { .. } => unreachable!(),
            })
            .collect();
        assert!(keys.contains(&(PlaceholderType::CbId, "b".to_string())));
        assert!(keys.contains(&(PlaceholderType::Custom, "c".to_string())));
    }

    #[test]
    fn placeholder_literal_round_trips() {
        let ph = Placeholder {
            ty: PlaceholderType::CbId,
            key: "cb_hero".into(),
            start: 0,
            end: 0,
        };
        assert_eq!(ph.literal(), "__BRAZESYNC.cb_id.cb_hero__");
    }

    #[test]
    fn duplicate_lid_aborts_with_dedicated_error() {
        let body = "<a>__BRAZESYNC.lid.cta__</a> <a>__BRAZESYNC.lid.cta__</a>";
        let map = lookup(&[(PlaceholderType::Lid, "cta", "ai8kexrxcp03")]);
        let err = resolve_placeholders(body, &map).unwrap_err();
        assert!(err.iter().any(|e| matches!(
            e,
            ResolutionError::DuplicateLidKey { key, occurrences }
                if key == "cta" && occurrences.len() == 2
        )));
    }

    #[test]
    fn duplicate_cb_id_is_not_an_error() {
        // cb_id / custom / global re-use is normal substitution per §5.
        let body = "{{cb.__BRAZESYNC.cb_id.x__}} {{cb.__BRAZESYNC.cb_id.x__}}";
        let map = lookup(&[(PlaceholderType::CbId, "x", "cb42")]);
        let out = resolve_placeholders(body, &map).unwrap();
        assert_eq!(out, "{{cb.cb42}} {{cb.cb42}}");
    }

    #[test]
    fn body_without_placeholders_passes_through() {
        let body = "no placeholders here";
        let map = BTreeMap::new();
        assert_eq!(resolve_placeholders(body, &map).unwrap(), body);
    }

    #[test]
    fn suspicious_catches_typo_with_underscore_key() {
        // Regression: loose regex used to use `[^_\s]+` which excluded
        // underscores, silently letting `__BRAZSYNC.lid.spring_sale__`
        // (missing `E`, underscored key) through without a warning.
        let body = "__BRAZSYNC.lid.spring_sale__";
        let warns = find_suspicious_placeholders(body);
        assert_eq!(warns, vec!["__BRAZSYNC.lid.spring_sale__".to_string()]);
    }

    #[test]
    fn does_not_swallow_text_across_envelope_boundary() {
        // Regression: a regex with a greedy `[a-z0-9_]*` key class merged
        // `__BRAZESYNC.lid.foo__hello__BRAZESYNC.lid.bar__` into one
        // placeholder with key=`foo__hello`. The parser must stop at the
        // nearest `__` so both placeholders are recovered.
        let body = "__BRAZESYNC.lid.foo__hello__BRAZESYNC.lid.bar__";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].key, "foo");
        assert_eq!(ps[1].key, "bar");
        assert_eq!(&body[ps[0].start..ps[0].end], "__BRAZESYNC.lid.foo__");
        assert_eq!(&body[ps[1].start..ps[1].end], "__BRAZESYNC.lid.bar__");
    }

    #[test]
    fn adjacent_placeholders_share_no_underscore() {
        // `____` between two placeholders: prior greedy regex captured
        // key=`foo__`. The parser should treat the inner `__` as the close.
        let body = "__BRAZESYNC.lid.foo____BRAZESYNC.lid.bar__";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].key, "foo");
        assert_eq!(ps[1].key, "bar");
    }

    #[test]
    fn trailing_underscore_key_extracts_when_envelope_appears_to_have_three_underscores() {
        // Regression for v0.14.2 templatize output: when a URL slug is
        // empty the fallback key is `link_`, and the rendered envelope
        // collapses to `___` (close `__` + trailing `_` from the key).
        // Greedy-rightmost parse must pick key=`link_`.
        let body = "lid: '__BRAZESYNC.lid.link___'";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].key, "link_");
        assert_eq!(ps[0].ty, PlaceholderType::Lid);
    }

    #[test]
    fn placeholder_followed_by_unrelated_double_underscore_token_does_not_absorb_it() {
        // Regression: an earlier right-most-close strategy would greedily
        // extend the key across any adjacent `[a-z0-9_]+__` token, e.g.
        // Python `__init__` or Markdown bold immediately after a
        // placeholder. The parser must stop at the nearest `__` close
        // (with at most one trailing `_` for legacy recovery) and leave
        // the rest of the body untouched.
        let body = "__BRAZESYNC.lid.foo____bar__";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].key, "foo");
        assert_eq!(&body[ps[0].end..], "__bar__");
    }

    #[test]
    fn non_legacy_key_followed_by_underscore_text_is_not_absorbed() {
        // Recovery is gated on the v0.14.2 empty-slug fallbacks
        // (`lid.link_`, `cb_id.cb_`); any other `(type, key)` must
        // leave a trailing `_<text>` in the surrounding body.
        let body = "__BRAZESYNC.custom.foo___bar";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].key, "foo");
        assert_eq!(ps[0].ty, PlaceholderType::Custom);
        assert_eq!(&body[ps[0].end..], "_bar");

        let body = "__BRAZESYNC.lid.other___tail";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].key, "other");
        assert_eq!(&body[ps[0].end..], "_tail");
    }

    #[test]
    fn cb_id_empty_slug_fallback_extracts_with_trailing_underscore() {
        let body = "__BRAZESYNC.cb_id.cb___";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].key, "cb_");
        assert_eq!(ps[0].ty, PlaceholderType::CbId);
    }

    #[test]
    fn unresolved_trailing_underscore_key_reports_full_key() {
        // If `link_` isn't in the values map, the error must name
        // `link_` (not `link`) so the operator can fix the right entry.
        let body = "__BRAZESYNC.lid.link___";
        let map = lookup(&[(PlaceholderType::Lid, "ok", "ai8kexrxcp03")]);
        let err = resolve_placeholders(body, &map).unwrap_err();
        assert!(err.iter().any(|e| matches!(
            e,
            ResolutionError::UnknownKey { key, .. } if key == "link_"
        )));
    }

    #[test]
    fn underscored_keys_still_extract() {
        // Sanity: legitimate underscored keys (RFC §2.3 allows them) are
        // not broken by the boundary-respecting parser.
        let body = "__BRAZESYNC.lid.spring_sale__ x __BRAZESYNC.custom.api_host__";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].key, "spring_sale");
        assert_eq!(ps[1].key, "api_host");
    }
}
