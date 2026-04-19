//! Custom Attribute endpoints.
//!
//! Custom Attributes are managed in **registry mode**: the only list
//! endpoint is `GET /custom_attributes`, and the only mutation is
//! toggling the `blocklisted` (deprecated) flag.
//!
//! Braze creates Custom Attributes implicitly when `/users/track`
//! receives data containing a previously-unseen attribute name. There
//! is no declarative "create attribute" API.
//!
//! ## Wire contract
//!
//! Pagination is cursor-based via the RFC 5988 `Link: rel="next"` header;
//! the response body does not carry the cursor. `limit` is not a
//! supported query parameter — page size is fixed at 50 server-side.
//! `deprecated` is derived from `status == STATUS_BLOCKLISTED`.

use crate::braze::error::BrazeApiError;
use crate::braze::{parse_next_link, BrazeClient};
use crate::resource::{CustomAttribute, CustomAttributeType};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// At Braze's fixed 50 items/page this covers 10k attributes.
const SAFETY_CAP_PAGES: usize = 200;

/// Wire value of the `status` field that indicates a deprecated attribute.
const STATUS_BLOCKLISTED: &str = "Blocklisted";

impl BrazeClient {
    /// List all Custom Attributes from Braze. Follows RFC 5988 `Link`
    /// headers through every page until the server stops returning
    /// `rel="next"`.
    pub async fn list_custom_attributes(&self) -> Result<Vec<CustomAttribute>, BrazeApiError> {
        let mut all: Vec<CustomAttribute> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut next_url: Option<String> = None;

        for _ in 0..SAFETY_CAP_PAGES {
            let req = match &next_url {
                None => self.get(&["custom_attributes"]),
                Some(url) => self.get_absolute(url)?,
            };
            let (resp, headers): (CustomAttributeListResponse, _) =
                self.send_json_with_headers(req).await?;

            // Dedup across pages — per-page checks would miss a name that
            // recurs on a later cursor page.
            for w in resp.attributes {
                if !seen.insert(w.name.clone()) {
                    return Err(BrazeApiError::DuplicateNameInListResponse {
                        endpoint: "/custom_attributes",
                        name: w.name,
                    });
                }
                all.push(wire_to_domain(w));
            }

            match parse_next_link(&headers) {
                // Guard against a server that echoes the same cursor —
                // without this the safety-cap is the only exit.
                Some(url) if Some(&url) == next_url.as_ref() => {
                    return Err(BrazeApiError::PaginationNotImplemented {
                        endpoint: "/custom_attributes",
                        detail: format!("server returned same next link twice: {url}"),
                    });
                }
                Some(url) => next_url = Some(url),
                None => return Ok(all),
            }
        }

        Err(BrazeApiError::PaginationNotImplemented {
            endpoint: "/custom_attributes",
            detail: format!("exceeded {SAFETY_CAP_PAGES} page safety cap"),
        })
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

fn wire_to_domain(w: CustomAttributeWire) -> CustomAttribute {
    CustomAttribute {
        name: w.name,
        attribute_type: wire_data_type_to_domain(w.data_type.as_deref()),
        description: w.description,
        deprecated: w
            .status
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case(STATUS_BLOCKLISTED))
            .unwrap_or(false),
    }
}

/// Map the Braze wire `data_type` string to our domain enum.
///
/// Braze returns values like `"String (Automatically Detected)"` — we
/// match on the **leading whitespace-delimited token** (case-insensitive)
/// to ignore the suffix. Unknown values default to `String` with a warn.
fn wire_data_type_to_domain(raw: Option<&str>) -> CustomAttributeType {
    let lowered = raw.unwrap_or("").to_ascii_lowercase();

    // `object_array` and `object array` are both observed in practice.
    // Check the two-token form first so "object" doesn't eat "object array".
    if lowered.starts_with("object array") || lowered.starts_with("object_array") {
        return CustomAttributeType::ObjectArray;
    }

    let leading = lowered.split_whitespace().next().unwrap_or("");
    match leading {
        "string" => CustomAttributeType::String,
        "number" | "integer" | "float" => CustomAttributeType::Number,
        "boolean" | "bool" => CustomAttributeType::Boolean,
        "time" | "date" => CustomAttributeType::Time,
        "array" => CustomAttributeType::Array,
        "object" => CustomAttributeType::Object,
        "" => {
            tracing::debug!("Braze data_type is absent, defaulting to string");
            CustomAttributeType::String
        }
        unknown => {
            tracing::warn!(
                data_type = unknown,
                raw = ?raw,
                "unknown Braze data_type, defaulting to string"
            );
            CustomAttributeType::String
        }
    }
}

#[derive(Debug, Deserialize)]
struct CustomAttributeListResponse {
    #[serde(default)]
    attributes: Vec<CustomAttributeWire>,
}

#[derive(Debug, Deserialize)]
struct CustomAttributeWire {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    data_type: Option<String>,
    /// `"Active"` or `"Blocklisted"`. Absent for older workspaces —
    /// treated as not blocklisted.
    #[serde(default)]
    status: Option<String>,
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
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "attributes": [
                    {
                        "name": "last_visit_date",
                        "description": "Most recent visit",
                        "data_type": "Date (Automatically Detected)",
                        "array_length": null,
                        "status": "Active",
                        "tag_names": []
                    },
                    {
                        "name": "legacy_segment",
                        "description": null,
                        "data_type": "String",
                        "array_length": null,
                        "status": "Blocklisted",
                        "tag_names": []
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
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"attributes": []})))
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
                "attributes": [{
                    "name": "foo",
                    "data_type": "String",
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
    async fn list_follows_link_header_through_pages() {
        let server = MockServer::start().await;
        let base = server.uri();
        let page_2_link = format!(
            "<{base}/custom_attributes?cursor=p2>; rel=\"next\"",
            base = base
        );

        // Page 2 (mounted first so cursor-bearing requests hit it).
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .and(wiremock::matchers::query_param("cursor", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "attributes": [
                    {"name": "c", "data_type": "String", "status": "Active"}
                ],
                "message": "success"
            })))
            .mount(&server)
            .await;
        // Page 1 — no cursor query param, carries a Link header to p2.
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", page_2_link.as_str())
                    .set_body_json(json!({
                        "attributes": [
                            {"name": "a", "data_type": "String", "status": "Active"},
                            {"name": "b", "data_type": "Number", "status": "Active"}
                        ],
                        "message": "success"
                    })),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        let attrs = client.list_custom_attributes().await.unwrap();
        assert_eq!(attrs.len(), 3);
        assert_eq!(attrs[0].name, "a");
        assert_eq!(attrs[2].name, "c");
    }

    #[tokio::test]
    async fn list_maps_data_types_correctly() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "attributes": [
                    {"name": "s", "data_type": "String (Automatically Detected)"},
                    {"name": "n", "data_type": "Number"},
                    {"name": "b", "data_type": "Boolean"},
                    {"name": "t", "data_type": "Date"},
                    {"name": "a", "data_type": "Array"},
                    {"name": "o", "data_type": "Object"},
                    {"name": "oa", "data_type": "Object Array"}
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
        assert_eq!(attrs[5].attribute_type, CustomAttributeType::Object);
        assert_eq!(attrs[6].attribute_type, CustomAttributeType::ObjectArray);
    }

    #[tokio::test]
    async fn deprecated_is_derived_from_status_blocklisted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "attributes": [
                    {"name": "active", "data_type": "String", "status": "Active"},
                    {"name": "blocked", "data_type": "String", "status": "Blocklisted"},
                    {"name": "missing", "data_type": "String"}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let attrs = client.list_custom_attributes().await.unwrap();
        assert!(!attrs[0].deprecated);
        assert!(attrs[1].deprecated);
        assert!(!attrs[2].deprecated);
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
                "attributes": [
                    {"name": "dup", "data_type": "String"},
                    {"name": "unique", "data_type": "Number"},
                    {"name": "dup", "data_type": "String"}
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
    async fn list_errors_on_duplicate_name_across_pages() {
        // Same name appears on page 1 and page 2 — must be detected even
        // though each individual page is internally unique.
        let server = MockServer::start().await;
        let base = server.uri();
        let page_2_link = format!("<{base}/custom_attributes?cursor=p2>; rel=\"next\"");

        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .and(wiremock::matchers::query_param("cursor", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "attributes": [
                    {"name": "dup", "data_type": "String", "status": "Active"}
                ]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", page_2_link.as_str())
                    .set_body_json(json!({
                        "attributes": [
                            {"name": "dup", "data_type": "String", "status": "Active"}
                        ]
                    })),
            )
            .up_to_n_times(1)
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
    async fn list_errors_when_cursor_repeats() {
        // Server echoes the same `rel="next"` cursor forever — without
        // cycle detection we'd loop to SAFETY_CAP_PAGES.
        let server = MockServer::start().await;
        let base = server.uri();
        let self_link = format!("<{base}/custom_attributes?cursor=loop>; rel=\"next\"");

        Mock::given(method("GET"))
            .and(path("/custom_attributes"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", self_link.as_str())
                    .set_body_json(json!({ "attributes": [] })),
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
