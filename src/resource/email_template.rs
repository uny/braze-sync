//! Email Template domain type. See IMPLEMENTATION.md §6.4.
//!
//! `body_plaintext` is always present (`String`, not `Option<String>`); the
//! empty string is the legitimate value for HTML-only templates. This is a
//! deliberate decision recorded in §17.
//!
//! API verification (2026-04-12):
//! - `from_address`, `from_display_name`, `reply_to` do NOT exist in Braze API
//! - `description` is returned by /info but NOT settable via create/update (read-only)
//! - `should_inline_css` is supported by create/update/info
//! - Braze field name mapping: `template_name`→`name`, `body`→`body_html`,
//!   `plaintext_body`→`body_plaintext`

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmailTemplate {
    pub name: String,
    pub subject: String,
    /// HTML body (may contain Liquid; treated as opaque text in v1.0).
    pub body_html: String,
    /// Plaintext fallback. Empty string allowed; field always present.
    #[serde(default)]
    pub body_plaintext: String,
    /// Returned by Braze /info but not settable via create/update.
    /// Excluded from syncable_eq (same pattern as ContentBlock `state`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preheader: Option<String>,
    /// CSS inline processing toggle. Supported by create/update/info.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub should_inline_css: Option<bool>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_template_yaml_roundtrip() {
        let t = EmailTemplate {
            name: "welcome".into(),
            subject: "Welcome".into(),
            body_html: "<p>hi</p>".into(),
            body_plaintext: "hi".into(),
            description: Some("Welcome email".into()),
            preheader: Some("Get started".into()),
            should_inline_css: Some(true),
            tags: vec!["onboarding".into()],
        };
        let yaml = serde_norway::to_string(&t).unwrap();
        let parsed: EmailTemplate = serde_norway::from_str(&yaml).unwrap();
        assert_eq!(t, parsed);
    }

    #[test]
    fn empty_plaintext_is_valid() {
        let t = EmailTemplate {
            name: "html_only".into(),
            subject: "x".into(),
            body_html: "<p>x</p>".into(),
            body_plaintext: String::new(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: vec![],
        };
        let yaml = serde_norway::to_string(&t).unwrap();
        let parsed: EmailTemplate = serde_norway::from_str(&yaml).unwrap();
        assert_eq!(parsed.body_plaintext, "");
    }
}
