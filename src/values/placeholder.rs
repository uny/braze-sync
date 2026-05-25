//! Anonymous Braze-managed placeholder: `__BRAZESYNC__`.
//!
//! v0.16 model: a single anonymous token represents both `lid` and
//! `cb_id` placeholders. Type is inferred from the surrounding
//! `| lid: '…'` / `| id: '…'` syntax. Resolution is offset-based and
//! lives in [`crate::values::braze_managed`].

use regex_lite::Regex;
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
}

/// One `__BRAZESYNC__` occurrence with its inferred type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placeholder {
    /// `None` when the token is not in a recognized `| lid:` / `| id:`
    /// argument position — caller treats this as a fatal context error.
    pub ty: Option<PlaceholderType>,
    /// Byte offset where the literal `__BRAZESYNC__` token begins.
    pub start: usize,
    /// Byte offset (exclusive) where it ends.
    pub end: usize,
}

pub const TOKEN: &str = "__BRAZESYNC__";

/// Resolution errors. All offset-based — no keys exist in the v0.16
/// model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolutionError {
    /// lid `__BRAZESYNC__` could not be resolved from the remote body.
    UnresolvedLid {
        start: usize,
        anchor: Option<String>,
    },
    /// cb_id `__BRAZESYNC__` could not be matched to a remote
    /// `${NAME} | id: 'cbN'`.
    UnresolvedCbId {
        start: usize,
        name: Option<String>,
    },
    /// `__BRAZESYNC__` appeared outside any recognized
    /// `| lid:` / `| id:` argument position.
    UnknownContext { start: usize },
    /// Token uses the retired `__BRAZESYNC.<…>__` envelope (legacy v0.15
    /// syntax, or `custom`/`global` namespaces removed earlier). Surface
    /// as fatal so operators re-run `templatize`.
    RetiredNamespace { token: String },
}

fn lid_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\|\s*lid:\s*['"]$"#).expect("lid prefix regex is valid"))
}

fn cb_id_prefix_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\|\s*id:\s*['"]$"#).expect("cb_id prefix regex is valid"))
}

/// Decide which type a `__BRAZESYNC__` token represents based on the
/// text immediately preceding it.
pub fn infer_type(prefix: &str) -> Option<PlaceholderType> {
    if lid_prefix_re().is_match(prefix) {
        return Some(PlaceholderType::Lid);
    }
    if cb_id_prefix_re().is_match(prefix) {
        return Some(PlaceholderType::CbId);
    }
    None
}

/// Extract every `__BRAZESYNC__` occurrence in `body` with its inferred
/// type.
pub fn extract_placeholders(body: &str) -> Vec<Placeholder> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(rel) = body[i..].find(TOKEN) {
        let start = i + rel;
        let end = start + TOKEN.len();
        let ty = infer_type(&body[..start]);
        out.push(Placeholder { ty, start, end });
        i = end;
    }
    out
}

/// Cheap check used by callers that only need to know whether
/// resolution must run at all.
pub fn has_placeholders(body: &str) -> bool {
    body.contains(TOKEN)
}

/// Loose envelope regex for the **retired** v0.15 syntax. We surface
/// these as errors so operators re-run `templatize` rather than
/// silently shipping unresolvable templates.
fn retired_envelope_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"__BRAZE?SYNC\.[A-Za-z0-9_]+\.[A-Za-z0-9_]+__")
            .expect("retired envelope regex is valid")
    })
}

/// Loose typo regex (e.g. `__BRAZSYNC__`, missing E). Excludes the
/// strict token itself and the retired envelope shape.
fn typo_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"__BRAZE?SYNC[A-Z]*__").expect("typo token regex is valid")
    })
}

/// Find any token resembling a placeholder that is NOT the strict
/// `__BRAZESYNC__`. Returns retired-envelope and typo-shaped strings.
pub fn find_suspicious_placeholders(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for m in retired_envelope_re().find_iter(body) {
        out.push(m.as_str().to_string());
    }
    for m in typo_token_re().find_iter(body) {
        let s = m.as_str();
        if s == TOKEN {
            continue;
        }
        // Skip if already covered by a retired-envelope hit at the same
        // start offset.
        if out.iter().any(|o: &String| o.starts_with(s)) {
            continue;
        }
        out.push(s.to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_anonymous_lid_token() {
        let body = "x | lid: '__BRAZESYNC__' y";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].ty, Some(PlaceholderType::Lid));
    }

    #[test]
    fn extracts_anonymous_cb_id_token() {
        let body = "x | id: '__BRAZESYNC__' y";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].ty, Some(PlaceholderType::CbId));
    }

    #[test]
    fn double_quoted_context_recognized() {
        let body = r#"| lid: "__BRAZESYNC__""#;
        let ps = extract_placeholders(body);
        assert_eq!(ps[0].ty, Some(PlaceholderType::Lid));
    }

    #[test]
    fn token_without_filter_context_has_none_type() {
        let body = "bare __BRAZESYNC__ token";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 1);
        assert!(ps[0].ty.is_none());
    }

    #[test]
    fn token_outside_quotes_has_none_type() {
        let body = "| lid: __BRAZESYNC__";
        let ps = extract_placeholders(body);
        assert!(ps[0].ty.is_none());
    }

    #[test]
    fn multiple_tokens_in_order() {
        let body = "a | lid: '__BRAZESYNC__' b | id: '__BRAZESYNC__' c";
        let ps = extract_placeholders(body);
        assert_eq!(ps.len(), 2);
        assert_eq!(ps[0].ty, Some(PlaceholderType::Lid));
        assert_eq!(ps[1].ty, Some(PlaceholderType::CbId));
        assert!(ps[0].start < ps[1].start);
    }

    #[test]
    fn retired_envelope_is_suspicious() {
        let body = "stuff __BRAZESYNC.lid.foo__ stuff";
        let warns = find_suspicious_placeholders(body);
        assert_eq!(warns, vec!["__BRAZESYNC.lid.foo__".to_string()]);
    }

    #[test]
    fn retired_custom_namespace_is_suspicious() {
        let body = "x __BRAZESYNC.custom.foo__ y";
        let warns = find_suspicious_placeholders(body);
        assert_eq!(warns, vec!["__BRAZESYNC.custom.foo__".to_string()]);
    }

    #[test]
    fn typo_token_is_suspicious() {
        let body = "x __BRAZSYNC__ y";
        let warns = find_suspicious_placeholders(body);
        assert!(warns.iter().any(|w| w.contains("BRAZSYNC")));
    }

    #[test]
    fn strict_token_is_not_suspicious() {
        let body = "x __BRAZESYNC__ y";
        assert!(find_suspicious_placeholders(body).is_empty());
    }
}
