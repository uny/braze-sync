//! Custom Attribute endpoints.
//!
//! Custom Attributes are managed in **registry mode**: the only list
//! endpoint is `GET /custom_attributes`, and the only mutation is
//! toggling the `blocklisted` (deprecated) flag.
//!
//! Braze creates Custom Attributes implicitly when `/users/track`
//! receives data containing a previously-unseen attribute name. There
//! is no declarative "create attribute" API.

use crate::braze::error::BrazeApiError;
use crate::braze::{check_duplicate_names, check_pagination, BrazeClient};
use crate::resource::{CustomAttribute, CustomAttributeType};
use serde::{Deserialize, Serialize};

const LIST_LIMIT: u32 = 100;

impl BrazeClient {
    /// List all Custom Attributes from Braze. Fail-closed on pagination:
    /// if the response indicates more attributes exist beyond the first
    /// page, returns `PaginationNotImplemented` rather than silently
    /// truncating.
    pub async fn list_custom_attributes(&self) -> Result<Vec<CustomAttribute>, BrazeApiError> {
        let req = self
            .get(&["custom_attributes"])
            .query(&[("limit", LIST_LIMIT.to_string())]);
        let resp: CustomAttributeListResponse = self.send_json(req).await?;
        let returned = resp.custom_attributes.len();

        // Fail-closed pagination guard.
        check_pagination(
            resp.count,
            returned,
            LIST_LIMIT as usize,
            "/custom_attributes",
        )?;

        check_duplicate_names(
            resp.custom_attributes.iter().map(|e| e.custom_attribute_name.as_str()),
            returned,
            "/custom_attributes",
        )?;

        Ok(resp
            .custom_attributes
            .into_iter()
            .map(|w| CustomAttribute {
                name: w.custom_attribute_name,
                attribute_type: wire_data_type_to_domain(w.data_type.as_deref()),
                description: w.description,
                // Braze omits `blocklisted` for non-blocklisted attributes;
                // treat absent as active (not deprecated).
                deprecated: w.blocklisted.unwrap_or(false),
            })
            .collect())
    }

    /// Toggle the `blocklisted` (deprecated) flag on one or more Custom
    /// Attributes. This is the **only** write operation braze-sync
    /// performs for Custom Attributes.
    pub async fn set_custom_attribute_blocklist(
        &self,
        names: &[&str],
        blocklisted: bool,
    ) -> Result<(), BrazeApiError> {
        let body = BlocklistRequest {
            custom_attribute_names: names,
            blocklisted,
        };
        let req = self.post(&["custom_attributes", "blocklist"]).json(&body);
        self.send_ok(req).await
    }
}

/// Map the Braze wire `data_type` string to our domain enum.
/// Unknown types default to `String` — forward-compat for types Braze
/// may add in the future.
fn wire_data_type_to_domain(data_type: Option<&str>) -> CustomAttributeType {
    match data_type {
        Some("string") => CustomAttributeType::String,
        // Braze docs list "integer" and "float"; "number" is not
        // documented but included defensively in case the API ever
        // returns it as an alias.
        Some("integer") | Some("float") | Some("number") => CustomAttributeType::Number,
        // "bool" is not documented either but guarded for the same reason.
        Some("boolean") | Some("bool") => CustomAttributeType::Boolean,
        Some("date") | Some("time") => CustomAttributeType::Time,
        Some("array") => CustomAttributeType::Array,
        Some(unknown) => {
            tracing::warn!(
                data_type = unknown,
                "unknown Braze data_type, defaulting to string"
            );
            CustomAttributeType::String
        }
        None => {
            tracing::debug!("Braze data_type is absent, defaulting to string");
            CustomAttributeType::String
        }
    }
}

// =====================================================================
// Wire types — Braze API response shapes.
// =====================================================================

#[derive(Debug, Deserialize)]
struct CustomAttributeListResponse {
    #[serde(default)]
    custom_attributes: Vec<CustomAttributeWire>,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct CustomAttributeWire {
    #[serde(default)]
    custom_attribute_name: String,
    #[serde(default)]
    data_type: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    blocklisted: Option<bool>,
}

#[derive(Debug, Serialize)]
struct BlocklistRequest<'a> {
    custom_attribute_names: &'a [&'a str],
    blocklisted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::braze::test_client as make_client;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .and(header("authorization", "Bearer test-key"))
            .and(query_param("limit", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2,
                "custom_attributes": [
                    {
                        "custom_attribute_name": "last_visit_date",
                        "data_type": "date",
                        "description": "Most recent visit",
                        "blocklisted": false
                    },
                    {
                        "custom_attribute_name": "legacy_segment",
                        "data_type": "string",
                        "blocklisted": true
                    }
                ],
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let attrs = client.list_custom_attributes().await.unwrap();
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].name, "last_visit_date");
        assert_eq!(attrs[0].attribute_type, CustomAttributeType::Time);
        assert_eq!(attrs[0].description.as_deref(), Some("Most recent visit"));
        assert!(!attrs[0].deprecated);
        assert_eq!(attrs[1].name, "legacy_segment");
        assert!(attrs[1].deprecated);
    }

    #[tokio::test]
    async fn list_empty_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"custom_attributes": []})),
            )
            .mount(&server)
            .await;
        let client = make_client(&server);
        assert!(client.list_custom_attributes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_ignores_unknown_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "custom_attributes": [{
                    "custom_attribute_name": "foo",
                    "data_type": "string",
                    "future_field": "ignored"
                }]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let attrs = client.list_custom_attributes().await.unwrap();
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs[0].name, "foo");
    }

    #[tokio::test]
    async fn list_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_custom_attributes().await.unwrap_err();
        assert!(matches!(err, BrazeApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn list_errors_when_count_exceeds_returned() {
        let server = MockServer::start().await;
        let entries: Vec<serde_json::Value> = (0..50)
            .map(|i| {
                json!({
                    "custom_attribute_name": format!("attr_{i}"),
                    "data_type": "string"
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 150,
                "custom_attributes": entries,
                "message": "success"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_custom_attributes().await.unwrap_err();
        assert!(
            matches!(err, BrazeApiError::PaginationNotImplemented { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn list_errors_on_full_page_with_no_count_field() {
        let server = MockServer::start().await;
        let entries: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                json!({
                    "custom_attribute_name": format!("attr_{i}"),
                    "data_type": "string"
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "custom_attributes": entries })),
            )
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_custom_attributes().await.unwrap_err();
        assert!(
            matches!(err, BrazeApiError::PaginationNotImplemented { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn list_short_page_with_no_count_is_trusted_as_complete() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "custom_attributes": [
                    {"custom_attribute_name": "a", "data_type": "string"},
                    {"custom_attribute_name": "b", "data_type": "number"}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let attrs = client.list_custom_attributes().await.unwrap();
        assert_eq!(attrs.len(), 2);
    }

    #[tokio::test]
    async fn list_maps_data_types_correctly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 5,
                "custom_attributes": [
                    {"custom_attribute_name": "s", "data_type": "string"},
                    {"custom_attribute_name": "n", "data_type": "integer"},
                    {"custom_attribute_name": "b", "data_type": "boolean"},
                    {"custom_attribute_name": "t", "data_type": "date"},
                    {"custom_attribute_name": "a", "data_type": "array"}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let attrs = client.list_custom_attributes().await.unwrap();
        assert_eq!(attrs[0].attribute_type, CustomAttributeType::String);
        assert_eq!(attrs[1].attribute_type, CustomAttributeType::Number);
        assert_eq!(attrs[2].attribute_type, CustomAttributeType::Boolean);
        assert_eq!(attrs[3].attribute_type, CustomAttributeType::Time);
        assert_eq!(attrs[4].attribute_type, CustomAttributeType::Array);
    }

    #[tokio::test]
    async fn blocklist_sends_correct_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/custom_attributes/blocklist"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(json!({
                "custom_attribute_names": ["legacy_segment"],
                "blocklisted": true
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": "success"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        client
            .set_custom_attribute_blocklist(&["legacy_segment"], true)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_errors_on_duplicate_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 3,
                "custom_attributes": [
                    {"custom_attribute_name": "dup", "data_type": "string"},
                    {"custom_attribute_name": "unique", "data_type": "number"},
                    {"custom_attribute_name": "dup", "data_type": "string"}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_custom_attributes().await.unwrap_err();
        match err {
            BrazeApiError::DuplicateNameInListResponse { endpoint, name } => {
                assert_eq!(endpoint, "/custom_attributes");
                assert_eq!(name, "dup");
            }
            other => panic!("expected DuplicateNameInListResponse, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn blocklist_unblocklist() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/custom_attributes/blocklist"))
            .and(body_json(json!({
                "custom_attribute_names": ["reactivated"],
                "blocklisted": false
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": "success"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        client
            .set_custom_attribute_blocklist(&["reactivated"], false)
            .await
            .unwrap();
    }
}
