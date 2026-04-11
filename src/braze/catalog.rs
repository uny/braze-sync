//! Catalog Schema endpoints. See IMPLEMENTATION.md §8.3.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::resource::{Catalog, CatalogField, CatalogFieldType};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

/// Wire shape of `GET /catalogs` and `GET /catalogs/{name}` responses.
///
/// **ASSUMED** based on IMPLEMENTATION.md §8.3 and Braze public docs.
/// If the actual shape differs, only this struct and the wrapping
/// logic in this file need to change.
///
/// Fields use serde defaults so an unexpected-but-related shape from
/// Braze (e.g. an extra status field) doesn't break parsing.
#[derive(Debug, Deserialize)]
struct CatalogsResponse {
    #[serde(default)]
    catalogs: Vec<Catalog>,
    /// Pagination cursor returned by Braze when more pages exist.
    /// Its presence is the signal we use to fail closed — see
    /// `list_catalogs`.
    #[serde(default)]
    next_cursor: Option<String>,
}

impl BrazeClient {
    /// `GET /catalogs` — list every catalog schema in the workspace.
    ///
    /// Sends a single request. Pagination is not yet implemented, so
    /// if Braze reports a `next_cursor` the client hard-fails with
    /// [`BrazeApiError::PaginationNotImplemented`] rather than silently
    /// returning page 1. The previous v0.2.0 behavior was to log a
    /// warning and keep going, which let `apply` on a >1-page workspace
    /// re-create the page-2 catalogs and mis-report drift against them.
    /// Fail-closed matches the pattern established by
    /// `list_content_blocks`.
    pub async fn list_catalogs(&self) -> Result<Vec<Catalog>, BrazeApiError> {
        let req = self.get(&["catalogs"]);
        let resp: CatalogsResponse = self.send_json(req).await?;
        if let Some(cursor) = resp.next_cursor.as_deref() {
            if !cursor.is_empty() {
                return Err(BrazeApiError::PaginationNotImplemented {
                    endpoint: "/catalogs",
                    detail: format!(
                        "got {} catalog(s) plus a non-empty next_cursor; \
                         aborting to prevent silent truncation",
                        resp.catalogs.len()
                    ),
                });
            }
        }
        Ok(resp.catalogs)
    }

    /// `GET /catalogs/{name}` — fetch a single catalog schema.
    ///
    /// 404 from Braze and an empty `catalogs` array in the response are
    /// both mapped to [`BrazeApiError::NotFound`] so callers can branch
    /// on "this catalog doesn't exist" without string matching on the
    /// HTTP body.
    pub async fn get_catalog(&self, name: &str) -> Result<Catalog, BrazeApiError> {
        let req = self.get(&["catalogs", name]);
        match self.send_json::<CatalogsResponse>(req).await {
            Ok(resp) => resp
                .catalogs
                .into_iter()
                .next()
                .ok_or_else(|| BrazeApiError::NotFound {
                    resource: format!("catalog '{name}'"),
                }),
            Err(BrazeApiError::Http { status, .. }) if status == StatusCode::NOT_FOUND => {
                Err(BrazeApiError::NotFound {
                    resource: format!("catalog '{name}'"),
                })
            }
            Err(e) => Err(e),
        }
    }

    /// `POST /catalogs/{name}/fields` — add one field to a catalog schema.
    ///
    /// **ASSUMED** wire format `{"fields": [{"name": "...", "type": "..."}]}`
    /// per IMPLEMENTATION.md §8.3 + Braze public docs. v0.1.0 sends one
    /// POST per added field.
    pub async fn add_catalog_field(
        &self,
        catalog_name: &str,
        field: &CatalogField,
    ) -> Result<(), BrazeApiError> {
        let body = AddFieldsRequest {
            fields: vec![WireField {
                name: &field.name,
                field_type: field.field_type,
            }],
        };
        let req = self.post(&["catalogs", catalog_name, "fields"]).json(&body);
        self.send_ok(req).await
    }

    /// `DELETE /catalogs/{name}/fields/{field}` — remove a field. **Destructive**.
    ///
    /// 404 from Braze stays as `Http { status: 404, .. }` rather than
    /// being mapped to `NotFound`. The use case is different from
    /// get_catalog: a 404 here means "the field you wanted to delete is
    /// already gone", which is a state-drift signal the user should see
    /// rather than silently no-op. A future `--ignore-missing` flag in
    /// `apply` can opt into idempotent behavior.
    pub async fn delete_catalog_field(
        &self,
        catalog_name: &str,
        field_name: &str,
    ) -> Result<(), BrazeApiError> {
        let req = self.delete(&["catalogs", catalog_name, "fields", field_name]);
        self.send_ok(req).await
    }
}

#[derive(Serialize)]
struct AddFieldsRequest<'a> {
    fields: Vec<WireField<'a>>,
}

#[derive(Serialize)]
struct WireField<'a> {
    name: &'a str,
    /// Reuses the domain type's snake_case `Serialize` impl so the
    /// wire string stays in sync with `CatalogFieldType` automatically.
    #[serde(rename = "type")]
    field_type: CatalogFieldType,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::braze::test_client as make_client;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn list_catalogs_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "catalogs": [
                    {
                        "name": "cardiology",
                        "description": "Cardiology catalog",
                        "fields": [
                            {"name": "id", "type": "string"},
                            {"name": "score", "type": "number"}
                        ]
                    },
                    {
                        "name": "dermatology",
                        "fields": [
                            {"name": "id", "type": "string"}
                        ]
                    }
                ],
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let cats = client.list_catalogs().await.unwrap();
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].name, "cardiology");
        assert_eq!(cats[0].description.as_deref(), Some("Cardiology catalog"));
        assert_eq!(cats[0].fields.len(), 2);
        assert_eq!(cats[0].fields[1].field_type, CatalogFieldType::Number);
        assert_eq!(cats[1].name, "dermatology");
        assert_eq!(cats[1].description, None);
    }

    #[tokio::test]
    async fn list_catalogs_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"catalogs": []})))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cats = client.list_catalogs().await.unwrap();
        assert!(cats.is_empty());
    }

    #[tokio::test]
    async fn list_catalogs_sets_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .and(header(
                "user-agent",
                concat!("braze-sync/", env!("CARGO_PKG_VERSION")),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"catalogs": []})))
            .mount(&server)
            .await;
        let client = make_client(&server);
        client.list_catalogs().await.unwrap();
    }

    #[tokio::test]
    async fn list_catalogs_ignores_unknown_fields_in_response() {
        // Forward compat: a future Braze response with extra fields
        // (both at the top level and inside catalog entries) should
        // still parse cleanly because no struct in the chain uses
        // deny_unknown_fields. `next_cursor` is deliberately NOT set
        // here — the cursor path has its own test
        // (`list_catalogs_errors_when_next_cursor_present`) so this
        // case stays focused on unknown-field tolerance alone.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "catalogs": [
                    {
                        "name": "future",
                        "description": "tomorrow",
                        "future_metadata": {"foo": "bar"},
                        "num_items": 1234,
                        "fields": [
                            {"name": "id", "type": "string", "extra": "ignored"}
                        ]
                    }
                ],
                "future_top_level": {"whatever": true},
                "message": "success"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cats = client.list_catalogs().await.unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].name, "future");
    }

    #[tokio::test]
    async fn list_catalogs_errors_when_next_cursor_present() {
        // Regression guard for the v0.2.0 silent-truncation bug:
        // a non-empty `next_cursor` must surface as
        // `PaginationNotImplemented` so that a workspace with >1 page
        // of catalogs cannot feed `apply` a partial view of remote
        // state. Empty-string cursor is tested separately below.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "catalogs": [
                    {"name": "cardiology", "fields": [{"name": "id", "type": "string"}]}
                ],
                "next_cursor": "abc123"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_catalogs().await.unwrap_err();
        match err {
            BrazeApiError::PaginationNotImplemented { endpoint, detail } => {
                assert_eq!(endpoint, "/catalogs");
                assert!(detail.contains("next_cursor"), "detail: {detail}");
                assert!(detail.contains("1 catalog"), "detail: {detail}");
            }
            other => panic!("expected PaginationNotImplemented, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_catalogs_empty_string_cursor_is_treated_as_no_more_pages() {
        // Some paginated APIs return `next_cursor: ""` on the last
        // page instead of omitting the field. Treat that as "no more
        // pages" rather than tripping the fail-closed guard — the
        // alternative would turn every workspace under one page into
        // an error.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "catalogs": [{"name": "only", "fields": []}],
                "next_cursor": ""
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cats = client.list_catalogs().await.unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].name, "only");
    }

    #[tokio::test]
    async fn unauthorized_returns_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid api key"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_catalogs().await.unwrap_err();
        assert!(matches!(err, BrazeApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn server_error_carries_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal explosion"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_catalogs().await.unwrap_err();
        match err {
            BrazeApiError::Http { status, body } => {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                assert!(body.contains("internal explosion"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn retries_on_429_and_succeeds() {
        let server = MockServer::start().await;
        // wiremock matches the *most recently mounted* mock first; the
        // limited 429 mock is mounted second so it preempts until used
        // up, after which the success mock takes over.
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "catalogs": [{"name": "after_retry", "fields": []}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        let cats = client.list_catalogs().await.unwrap();
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].name, "after_retry");
    }

    #[tokio::test]
    async fn retries_exhausted_returns_rate_limit_exhausted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_catalogs().await.unwrap_err();
        assert!(
            matches!(err, BrazeApiError::RateLimitExhausted),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn get_catalog_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs/cardiology"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "catalogs": [
                    {"name": "cardiology", "fields": [{"name": "id", "type": "string"}]}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cat = client.get_catalog("cardiology").await.unwrap();
        assert_eq!(cat.name, "cardiology");
        assert_eq!(cat.fields.len(), 1);
    }

    #[tokio::test]
    async fn get_catalog_404_is_mapped_to_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs/missing"))
            .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.get_catalog("missing").await.unwrap_err();
        match err {
            BrazeApiError::NotFound { resource } => assert!(resource.contains("missing")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_catalog_empty_response_array_is_not_found() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/catalogs/ghost"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"catalogs": []})))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.get_catalog("ghost").await.unwrap_err();
        assert!(matches!(err, BrazeApiError::NotFound { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn debug_does_not_leak_api_key() {
        let server = MockServer::start().await;
        let client = make_client(&server);
        let dbg = format!("{client:?}");
        assert!(!dbg.contains("test-key"), "leaked api key in: {dbg}");
        assert!(dbg.contains("<redacted>"));
    }

    #[tokio::test]
    async fn add_catalog_field_happy_path_sends_correct_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/catalogs/cardiology/fields"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(json!({
                "fields": [{"name": "severity_level", "type": "number"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let field = CatalogField {
            name: "severity_level".into(),
            field_type: CatalogFieldType::Number,
        };
        client
            .add_catalog_field("cardiology", &field)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn add_catalog_field_unauthorized_propagates() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/catalogs/cardiology/fields"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid key"))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let field = CatalogField {
            name: "x".into(),
            field_type: CatalogFieldType::String,
        };
        let err = client
            .add_catalog_field("cardiology", &field)
            .await
            .unwrap_err();
        assert!(matches!(err, BrazeApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn add_catalog_field_retries_on_429_then_succeeds() {
        let server = MockServer::start().await;
        // Success mounted first; the limited 429 mock is mounted second
        // and wiremock matches the most-recently-mounted one until it
        // exhausts its `up_to_n_times` budget.
        Mock::given(method("POST"))
            .and(path("/catalogs/cardiology/fields"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "ok"})))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/catalogs/cardiology/fields"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;

        let client = make_client(&server);
        let field = CatalogField {
            name: "x".into(),
            field_type: CatalogFieldType::String,
        };
        client
            .add_catalog_field("cardiology", &field)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_catalog_field_happy_path_uses_segment_encoded_path() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/catalogs/cardiology/fields/legacy_code"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&server)
            .await;

        let client = make_client(&server);
        client
            .delete_catalog_field("cardiology", "legacy_code")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn delete_catalog_field_server_error_returns_http() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/catalogs/cardiology/fields/x"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let err = client
            .delete_catalog_field("cardiology", "x")
            .await
            .unwrap_err();
        match err {
            BrazeApiError::Http { status, body } => {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                assert!(body.contains("oops"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }
}
