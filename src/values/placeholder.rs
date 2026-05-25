//! Placeholder extraction and resolution for `__BRAZESYNC.<type>.<key>__`.
//!
//! v0.15 model: only Braze-managed types (`lid`, `cb_id`) are
//! recognized. Both resolve at apply/diff time from the live remote
//! body via URL / `${NAME}` anchor correlation (see
//! [`crate::values::braze_managed`]).
//!
//! Syntax:
//!   - Double-underscore envelope: `__BRAZESYNC.…__`
//!   - Dot namespace
//!   - `<type>` ∈ {`lid`, `cb_id`}
//!   - `<key>` matches `^[a-z][a-z0-9_]*$`

use regex_lite::Regex;
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PlaceholderType {
    Lid,
    CbId,
}

impl PlaceholderType {
    pub fn as_str(&self) -> &'static str {
        match self {
            PlaceholderType::Lid => "lid",
            PlaceholderType::CbId => "cb_id",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "lid" => Some(Self::Lid),
            "cb_id" => Some(Self::CbId),
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

/// Loose envelope-only regex. Catches typos like `__BRAZSYNC.…__` or
/// unknown / retired types (`__BRAZESYNC.url.foo__`,
/// `__BRAZESYNC.custom.foo__`) so they surface as warnings rather than
/// silently passing through. Inner classes allow `_` so typo-shaped
/// placeholders whose key contains an underscore are still caught.
fn loose_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"__BRAZE?SYNC\.[A-Za-z0-9_]+\.[A-Za-z0-9_]+__")
            .expect("loose placeholder regex is valid")
    })
}

pub fn extract_placeholders(body: &str) -> Vec<Placeholder> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + PREFIX.len() <= body.len() {
        let Some(rel) = body[i..].find(PREFIX) else {
            break;
        };
        let start = i + rel;
        let inner_start = start + PREFIX.len();
        let Some(rel_close) = body[inner_start..].find(CLOSE) else {
            break;
        };
        let close_start = inner_start + rel_close;
        let end = close_start + CLOSE.len();
        let inner = &body[inner_start..close_start];
        if let Some((ty_str, key)) = inner.split_once('.') {
            if let (Some(ty), true) = (PlaceholderType::parse(ty_str), key_re().is_match(key)) {
                out.push(Placeholder {
                    ty,
                    key: key.to_string(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionError {
    UnknownKey {
        ty: PlaceholderType,
        key: String,
        start: usize,
    },
    /// lid is per-click-context — reuse of the same key is an error.
    DuplicateLidKey {
        key: String,
        occurrences: Vec<usize>,
    },
}

pub type LookupKey = (PlaceholderType, String);

/// Resolve every placeholder in `body` against `lookup`. Returns the
/// resolved body on success, or every unresolved placeholder on failure.
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
        let body = "before __BRAZESYNC.lid.cta__ middle __BRAZESYNC.cb_id.shared__ end";
        let map = lookup(&[
            (PlaceholderType::Lid, "cta", "ai8kexrxcp03"),
            (PlaceholderType::CbId, "shared", "cb42"),
        ]);
        let resolved = resolve_placeholders(body, &map).unwrap();
        assert_eq!(resolved, "before ai8kexrxcp03 middle cb42 end");
    }

    #[test]
    fn resolves_repeated_cb_id_to_same_value() {
        let body = "{{__BRAZESYNC.cb_id.shared__}}/a {{__BRAZESYNC.cb_id.shared__}}/b";
        let map = lookup(&[(PlaceholderType::CbId, "shared", "cb42")]);
        let resolved = resolve_placeholders(body, &map).unwrap();
        assert_eq!(resolved, "{{cb42}}/a {{cb42}}/b");
    }

    #[test]
    fn aggregates_unresolved_keys() {
        let body = "__BRAZESYNC.lid.a__ __BRAZESYNC.cb_id.b__ __BRAZESYNC.cb_id.c__";
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
        assert!(keys.contains(&(PlaceholderType::CbId, "c".to_string())));
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
        // cb_id re-use is normal (same block referenced twice).
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
    fn stops_at_nearest_close_envelope() {
        let body = "__BRAZESYNC.lid.foo____bar__";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].key, "foo");
        assert_eq!(&body[ps[0].end..], "__bar__");
    }

    #[test]
    fn triple_underscore_parses_as_key_plus_trailing() {
        // `__BRAZESYNC.lid.link___` → key=`link`, remaining `_`
        let body = "__BRAZESYNC.lid.link___";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].key, "link");
        assert_eq!(&body[ps[0].end..], "_");
    }

    #[test]
    fn underscored_keys_still_extract() {
        // Sanity: legitimate underscored keys are not broken by the
        // boundary-respecting parser.
        let body = "__BRAZESYNC.lid.spring_sale__ x __BRAZESYNC.cb_id.promo_banner__";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].key, "spring_sale");
        assert_eq!(ps[1].key, "promo_banner");
    }
}
