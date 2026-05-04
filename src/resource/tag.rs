//! Tags are managed in **registry mode**, derived from references on
//! other resources.
//!
//! Braze does not expose a public REST API for workspace tags — they
//! cannot be listed, created, updated, or deleted programmatically. Tags
//! exist only as embedded fields on other resources (content blocks,
//! email templates, campaigns, canvases). Workspace administrators
//! create tags through the Braze dashboard.
//!
//! braze-sync therefore tracks tags as a registry whose entries are
//! sourced from references on managed resources. The registry is the
//! GitOps contract: any tag a resource references must appear here, and
//! `apply` fails fast (before mutating any resource) when a referenced
//! tag is missing — instead of letting Braze return
//! `400 "The following Tags could not be found: [...]"` mid-pipeline.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TagRegistry {
    pub tags: Vec<Tag>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl TagRegistry {
    pub fn normalized(&self) -> Self {
        let mut sorted = self.clone();
        sorted.tags.sort_by(|a, b| a.name.cmp(&b.name));
        sorted
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tags.iter().any(|t| t.name == name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_yaml_roundtrip() {
        let r = TagRegistry {
            tags: vec![
                Tag {
                    name: "campaign".into(),
                    description: None,
                },
                Tag {
                    name: "ad_slot/dialog".into(),
                    description: Some("Dialog ad slot".into()),
                },
            ],
        };
        let yaml = serde_norway::to_string(&r).unwrap();
        let parsed: TagRegistry = serde_norway::from_str(&yaml).unwrap();
        assert_eq!(r, parsed);
    }

    #[test]
    fn normalized_sorts_tags_by_name() {
        let r = TagRegistry {
            tags: vec![
                Tag {
                    name: "z".into(),
                    description: None,
                },
                Tag {
                    name: "a".into(),
                    description: None,
                },
            ],
        };
        let n = r.normalized();
        assert_eq!(n.tags[0].name, "a");
        assert_eq!(n.tags[1].name, "z");
    }

    #[test]
    fn description_none_is_omitted_in_output() {
        let r = TagRegistry {
            tags: vec![Tag {
                name: "x".into(),
                description: None,
            }],
        };
        let yaml = serde_norway::to_string(&r).unwrap();
        assert!(!yaml.contains("description"));
    }

    #[test]
    fn contains_finds_existing_name() {
        let r = TagRegistry {
            tags: vec![Tag {
                name: "promo".into(),
                description: None,
            }],
        };
        assert!(r.contains("promo"));
        assert!(!r.contains("missing"));
    }
}
