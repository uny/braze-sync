//! Catalog Schema and Catalog Items domain types. See IMPLEMENTATION.md §6.2.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub fields: Vec<CatalogField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CatalogField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: CatalogFieldType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogFieldType {
    String,
    Number,
    Boolean,
    Time,
    Object,
    Array,
    /// Catch-all for field types not yet known to this binary version.
    /// Forward-compat: prevents deserialization failures when Braze adds
    /// new field types. Round-trips as `"unknown"` — the original type
    /// name is not preserved. Upgrade braze-sync for full support.
    #[serde(other)]
    Unknown,
}

impl CatalogFieldType {
    /// The lowercase wire string for this field type ("string", "number",
    /// ...). Single source of truth used by `format::table`,
    /// `format::json`, `cli::apply`, and `braze::catalog`. Matches the
    /// snake_case `Serialize` representation derived above so the wire
    /// string and the explicit method can never drift.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Number => "number",
            Self::Boolean => "boolean",
            Self::Time => "time",
            Self::Object => "object",
            Self::Array => "array",
            Self::Unknown => "unknown",
        }
    }
}

impl Catalog {
    /// Return a copy with `fields` sorted by name. Used to keep on-disk
    /// output and diff input deterministic regardless of API ordering.
    pub fn normalized(&self) -> Self {
        let mut sorted = self.clone();
        sorted.fields.sort_by(|a, b| a.name.cmp(&b.name));
        sorted
    }
}

/// Catalog Items are streamed: we keep an item-id → content-hash index in
/// memory (cheap, ~64 bytes/row) and only materialize the full rows when an
/// `apply` actually needs to write them. See IMPLEMENTATION.md §6.2 / §11.2.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogItems {
    pub catalog_name: String,
    /// item id → blake3 content hash of the normalized non-id field map.
    pub item_hashes: HashMap<String, String>,
    /// Materialized rows. `None` until a streaming reader populates them.
    pub rows: Option<Vec<CatalogItemRow>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogItemRow {
    pub id: String,
    /// Remaining fields, dynamic per parent catalog schema.
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

impl CatalogItemRow {
    /// blake3 hash of the canonical JSON of the non-id fields. Map keys are
    /// emitted in sorted order by `serde_json` for `serde_json::Map`, so the
    /// hash is independent of source field ordering.
    pub fn content_hash(&self) -> String {
        let canonical =
            serde_json::to_vec(&self.fields).expect("serde_json::Map serialization is infallible");
        blake3::hash(&canonical).to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_yaml_roundtrip() {
        let cat = Catalog {
            name: "cardiology".into(),
            description: Some("Cardiology catalog".into()),
            fields: vec![
                CatalogField {
                    name: "condition_id".into(),
                    field_type: CatalogFieldType::String,
                },
                CatalogField {
                    name: "display_order".into(),
                    field_type: CatalogFieldType::Number,
                },
            ],
        };
        let yaml = serde_norway::to_string(&cat).unwrap();
        let parsed: Catalog = serde_norway::from_str(&yaml).unwrap();
        assert_eq!(cat, parsed);
    }

    #[test]
    fn catalog_field_type_serializes_snake_case() {
        let yaml = serde_norway::to_string(&CatalogFieldType::Boolean).unwrap();
        assert_eq!(yaml.trim(), "boolean");
    }

    #[test]
    fn unknown_field_type_deserializes_without_failure() {
        // Forward compat: if Braze adds a field type this binary doesn't
        // know about, the catalog should still parse — the unknown type
        // round-trips as "unknown" rather than crashing the entire export.
        let yaml = "name: future\nfields:\n  - name: x\n    type: hyperlink\n";
        let cat: Catalog = serde_norway::from_str(yaml).unwrap();
        assert_eq!(cat.fields[0].field_type, CatalogFieldType::Unknown);
        assert_eq!(cat.fields[0].field_type.as_str(), "unknown");
    }

    #[test]
    fn unknown_field_type_does_not_break_known_fields() {
        // A catalog with a mix of known and unknown types should parse
        // the known types correctly.
        let yaml = "\
name: mixed
fields:
  - name: id
    type: string
  - name: fancy
    type: quantum_entanglement
  - name: score
    type: number
";
        let cat: Catalog = serde_norway::from_str(yaml).unwrap();
        assert_eq!(cat.fields.len(), 3);
        assert_eq!(cat.fields[0].field_type, CatalogFieldType::String);
        assert_eq!(cat.fields[1].field_type, CatalogFieldType::Unknown);
        assert_eq!(cat.fields[2].field_type, CatalogFieldType::Number);
    }

    #[test]
    fn description_omitted_when_none() {
        let cat = Catalog {
            name: "x".into(),
            description: None,
            fields: vec![],
        };
        let yaml = serde_norway::to_string(&cat).unwrap();
        assert!(!yaml.contains("description"));
    }

    #[test]
    fn normalized_sorts_fields_by_name() {
        let cat = Catalog {
            name: "x".into(),
            description: None,
            fields: vec![
                CatalogField {
                    name: "z".into(),
                    field_type: CatalogFieldType::String,
                },
                CatalogField {
                    name: "a".into(),
                    field_type: CatalogFieldType::String,
                },
            ],
        };
        let n = cat.normalized();
        assert_eq!(n.fields[0].name, "a");
        assert_eq!(n.fields[1].name, "z");
    }

    #[test]
    fn content_hash_is_field_order_independent() {
        let mut a = serde_json::Map::new();
        a.insert("name".into(), serde_json::json!("af"));
        a.insert("order".into(), serde_json::json!(1));

        let mut b = serde_json::Map::new();
        b.insert("order".into(), serde_json::json!(1));
        b.insert("name".into(), serde_json::json!("af"));

        let row_a = CatalogItemRow {
            id: "x".into(),
            fields: a,
        };
        let row_b = CatalogItemRow {
            id: "x".into(),
            fields: b,
        };
        assert_eq!(row_a.content_hash(), row_b.content_hash());
    }

    #[test]
    fn content_hash_changes_when_value_changes() {
        let mut a = serde_json::Map::new();
        a.insert("v".into(), serde_json::json!(1));
        let mut b = serde_json::Map::new();
        b.insert("v".into(), serde_json::json!(2));
        let ra = CatalogItemRow {
            id: "x".into(),
            fields: a,
        };
        let rb = CatalogItemRow {
            id: "x".into(),
            fields: b,
        };
        assert_ne!(ra.content_hash(), rb.content_hash());
    }
}
