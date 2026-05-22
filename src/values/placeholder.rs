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

/// Strict placeholder regex per RFC §2.3.
fn strict_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"__BRAZESYNC\.(lid|cb_id|custom|global)\.([a-z][a-z0-9_]*)__")
            .expect("strict placeholder regex is valid")
    })
}

/// Loose envelope-only regex per RFC §2.3 warning rule. Catches typos like
/// `__BRAZSYNC.…__` or unknown types like `__BRAZESYNC.url.foo__` so they
/// can be surfaced as warnings rather than silently passing through.
fn loose_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"__BRAZE?SYNC\.[^_\s]+\.[^_\s]+__")
            .expect("loose placeholder regex is valid")
    })
}

/// Extract every strict-matching `__BRAZESYNC.<type>.<key>__` occurrence
/// in `body`, in order of appearance.
pub fn extract_placeholders(body: &str) -> Vec<Placeholder> {
    strict_re()
        .captures_iter(body)
        .filter_map(|cap| {
            let whole = cap.get(0)?;
            let ty = PlaceholderType::parse(cap.get(1)?.as_str())?;
            let key = cap.get(2)?.as_str().to_string();
            Some(Placeholder {
                ty,
                key,
                start: whole.start(),
                end: whole.end(),
            })
        })
        .collect()
}

/// Find loose envelope matches that don't satisfy the strict pattern.
/// Caller surfaces these as warnings (RFC §2.3).
pub fn find_suspicious_placeholders(body: &str) -> Vec<String> {
    let strict_spans: Vec<(usize, usize)> = strict_re()
        .find_iter(body)
        .map(|m| (m.start(), m.end()))
        .collect();
    loose_re()
        .find_iter(body)
        .filter(|m| {
            !strict_spans
                .iter()
                .any(|&(s, e)| s == m.start() && e == m.end())
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
    fn body_without_placeholders_passes_through() {
        let body = "no placeholders here";
        let map = BTreeMap::new();
        assert_eq!(resolve_placeholders(body, &map).unwrap(), body);
    }
}
