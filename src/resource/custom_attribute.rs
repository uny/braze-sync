//! Custom Attributes are managed in **registry mode**.
//!
//! Braze creates Custom Attributes implicitly when `/users/track` receives
//! data containing a previously-unseen attribute name. There is no
//! declarative "create attribute" API. braze-sync therefore supports only:
//!
//! - `export`:   snapshot the current Braze attribute set into Git
//! - `diff`:     show drift between local registry and Braze
//! - `apply`:    toggle the deprecation flag — the *only* mutation
//! - `validate`: structural check of the local YAML registry
//!
//! New attributes are introduced by application code via `/users/track`,
//! never by braze-sync. See IMPLEMENTATION.md §2.2 / §6.5 / §11.5.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomAttributeRegistry {
    pub attributes: Vec<CustomAttribute>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomAttribute {
    pub name: String,
    #[serde(rename = "type")]
    pub attribute_type: CustomAttributeType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Marks the attribute deprecated. The only mutation `apply` performs.
    #[serde(default)]
    pub deprecated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomAttributeType {
    String,
    Number,
    Boolean,
    Time,
    Array,
}

impl CustomAttributeRegistry {
    pub fn normalized(&self) -> Self {
        let mut sorted = self.clone();
        sorted.attributes.sort_by(|a, b| a.name.cmp(&b.name));
        sorted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_yaml_roundtrip() {
        let r = CustomAttributeRegistry {
            attributes: vec![
                CustomAttribute {
                    name: "last_visit".into(),
                    attribute_type: CustomAttributeType::Time,
                    description: Some("Most recent visit".into()),
                    deprecated: false,
                },
                CustomAttribute {
                    name: "legacy_segment".into(),
                    attribute_type: CustomAttributeType::String,
                    description: None,
                    deprecated: true,
                },
            ],
        };
        let yaml = serde_yml::to_string(&r).unwrap();
        let parsed: CustomAttributeRegistry = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(r, parsed);
    }

    #[test]
    fn deprecated_defaults_to_false() {
        let yaml = "name: foo\ntype: string\n";
        let attr: CustomAttribute = serde_yml::from_str(yaml).unwrap();
        assert!(!attr.deprecated);
    }

    #[test]
    fn normalized_sorts_attributes_by_name() {
        let r = CustomAttributeRegistry {
            attributes: vec![
                CustomAttribute {
                    name: "z".into(),
                    attribute_type: CustomAttributeType::String,
                    description: None,
                    deprecated: false,
                },
                CustomAttribute {
                    name: "a".into(),
                    attribute_type: CustomAttributeType::String,
                    description: None,
                    deprecated: false,
                },
            ],
        };
        let n = r.normalized();
        assert_eq!(n.attributes[0].name, "a");
        assert_eq!(n.attributes[1].name, "z");
    }
}
