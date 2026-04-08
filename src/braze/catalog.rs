//! Catalog Schema endpoints. See IMPLEMENTATION.md §8.3.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::resource::Catalog;
use reqwest::StatusCode;
use serde::Deserialize;

/// Wire shape of `GET /catalogs` and `GET /catalogs/{name}` responses.
///
/// **ASSUMED** based on IMPLEMENTATION.md §8.3 and Braze public docs.
/// Phase C E2E tests against a real Braze sandbox will validate (and
/// adjust if needed) before v1.0 freeze. If the actual shape differs,
/// only this struct and the small wrapping logic in this file need to
/// change — the public surface (`list_catalogs`, `get_catalog`) is
/// stable.
///
/// Fields use serde defaults so an unexpected-but-related shape from
/// Braze (e.g. an extra status field) doesn't break parsing.
#[derive(Debug, Deserialize)]
struct CatalogsResponse {
    #[serde(default)]
    catalogs: Vec<Catalog>,
    #[serde(default)]
    #[allow(dead_code)]
    message: Option<String>,
}

impl BrazeClient {
    /// `GET /catalogs` — list every catalog schema in the workspace.
    ///
    /// v0.1.0 sends a single request and returns the first page. The
    /// Braze catalog API supports cursor-based pagination, but
    /// pagination handling lands in Phase C alongside scale validation.
    /// Workspaces with very large numbers of catalogs may see truncated
    /// results until then; the README and CLI help (A6) flag this.
    pub async fn list_catalogs(&self) -> Result<Vec<Catalog>, BrazeApiError> {
        let req = self.get(&["catalogs"]);
        let resp: CatalogsResponse = self.send_json(req).await?;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::CatalogFieldType;
    use secrecy::SecretString;
    use serde_json::json;
    use url::Url;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> BrazeClient {
        BrazeClient::new(
            Url::parse(&server.uri()).unwrap(),
            SecretString::from("test-key".to_string()),
            // Very high rpm so the limiter is effectively a no-op in tests.
            10_000,
        )
    }

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
        // deny_unknown_fields.
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
                "next_cursor": "abc",
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
}
