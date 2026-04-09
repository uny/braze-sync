//! Email Template domain type. See IMPLEMENTATION.md §6.4.
//!
//! `body_plaintext` is always present (`String`, not `Option<String>`); the
//! empty string is the legitimate value for HTML-only templates. This is a
//! deliberate decision recorded in §17.

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preheader: Option<String>,
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
            from_address: Some("noreply@example.com".into()),
            from_display_name: Some("Example".into()),
            reply_to: None,
            preheader: Some("Get started".into()),
            tags: vec!["onboarding".into()],
        };
        let yaml = serde_yml::to_string(&t).unwrap();
        let parsed: EmailTemplate = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(t, parsed);
    }

    #[test]
    fn empty_plaintext_is_valid() {
        let t = EmailTemplate {
            name: "html_only".into(),
            subject: "x".into(),
            body_html: "<p>x</p>".into(),
            body_plaintext: String::new(),
            from_address: None,
            from_display_name: None,
            reply_to: None,
            preheader: None,
            tags: vec![],
        };
        let yaml = serde_yml::to_string(&t).unwrap();
        let parsed: EmailTemplate = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(parsed.body_plaintext, "");
    }
}
