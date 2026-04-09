//! Content Block domain type. See IMPLEMENTATION.md §6.3.
//!
//! Liquid template bodies are treated as opaque text in v1.0; syntax
//! validation is deferred to the server (§7.6).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentBlock {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Liquid template body. Opaque text in v1.0.
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub state: ContentBlockState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentBlockState {
    Active,
    Draft,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_block_yaml_roundtrip() {
        let cb = ContentBlock {
            name: "promo".into(),
            description: Some("Promo banner".into()),
            content: "{{ user.${first_name} }}".into(),
            tags: vec!["pr".into()],
            state: ContentBlockState::Active,
        };
        let yaml = serde_yml::to_string(&cb).unwrap();
        let parsed: ContentBlock = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(cb, parsed);
    }

    #[test]
    fn state_serializes_snake_case() {
        let s = serde_yml::to_string(&ContentBlockState::Draft).unwrap();
        assert_eq!(s.trim(), "draft");
    }
}
