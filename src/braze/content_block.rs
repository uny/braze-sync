//! Content Block endpoints. See IMPLEMENTATION.md §8.3.
//!
//! Braze content blocks are referenced by `name` from Liquid templates,
//! but the API identifies them by `content_block_id` (a UUID-like
//! string). braze-sync stores only the name in local files; the id is a
//! transient runtime concept used to translate "the user wants to
//! update content block X" into "POST /content_blocks/update with
//! content_block_id=...".
//!
//! There is **no DELETE endpoint** in the Braze content blocks API,
//! which is exactly why braze-sync has to express remote-only blocks as
//! orphans (§11.6) instead of pretending it can drop them.
//!
//! Wire shapes here are **ASSUMED** based on Braze public docs +
//! IMPLEMENTATION.md §8.3 and pinned by wiremock contract tests in this
//! file. Phase C E2E gate (PHASE_A_NOTES.md §6) is responsible for
//! confirming them against a real sandbox.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::resource::{ContentBlock, ContentBlockState};
use serde::{Deserialize, Serialize};

/// `GET /content_blocks/list` request: v0.2.0 always asks for a single
/// large page. Pagination support lands in Phase C.
const LIST_LIMIT: u32 = 100;

/// Lightweight summary returned by `/content_blocks/list`. Carries
/// just enough to build a name → id index for the apply path; fetching
/// full content requires a follow-up `/content_blocks/info` call.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentBlockSummary {
    pub content_block_id: String,
    pub name: String,
}

impl BrazeClient {
    /// `GET /content_blocks/list` — enumerate every content block in the
    /// workspace. Returns one page (size [`LIST_LIMIT`]); workspaces with
    /// more blocks may see truncated results until Phase C pagination.
    pub async fn list_content_blocks(&self) -> Result<Vec<ContentBlockSummary>, BrazeApiError> {
        let req = self
            .get(&["content_blocks", "list"])
            .query(&[("limit", LIST_LIMIT.to_string())]);
        let resp: ContentBlockListResponse = self.send_json(req).await?;
        if let Some(count) = resp.count {
            if count > resp.content_blocks.len() {
                tracing::warn!(
                    returned = resp.content_blocks.len(),
                    total = count,
                    "Braze reported more content blocks than this page returned; \
                     pagination is not yet implemented (v0.2.0)"
                );
            }
        }
        Ok(resp
            .content_blocks
            .into_iter()
            .map(|w| ContentBlockSummary {
                content_block_id: w.content_block_id,
                name: w.name,
            })
            .collect())
    }

    /// `GET /content_blocks/info?content_block_id=...` — fetch one
    /// content block's full body and metadata. Returns the domain
    /// [`ContentBlock`] directly.
    ///
    /// Braze responds 200 with a non-success `message` field for "this
    /// id doesn't exist" rather than a 404; that case is mapped to
    /// [`BrazeApiError::NotFound`] so callers can branch consistently
    /// with the catalog client.
    pub async fn get_content_block(&self, id: &str) -> Result<ContentBlock, BrazeApiError> {
        let req = self
            .get(&["content_blocks", "info"])
            .query(&[("content_block_id", id)]);
        let wire: ContentBlockInfoResponse = self.send_json(req).await?;
        if !wire.is_success() {
            return Err(BrazeApiError::NotFound {
                resource: format!("content_block id '{id}'"),
            });
        }
        Ok(ContentBlock {
            name: wire.name,
            description: wire.description,
            content: wire.content,
            tags: wire.tags,
            // Braze content_blocks API does not expose a state concept;
            // default to Active so the round-trip is stable. See README
            // v0.2.0 limitations + diff/content_block.rs syncable_eq.
            state: ContentBlockState::Active,
        })
    }

    /// `POST /content_blocks/create` — create a new content block.
    /// Returns the newly assigned `content_block_id`.
    pub async fn create_content_block(&self, cb: &ContentBlock) -> Result<String, BrazeApiError> {
        let body = ContentBlockWriteBody {
            content_block_id: None,
            name: &cb.name,
            description: cb.description.as_deref(),
            content: &cb.content,
            tags: &cb.tags,
            state: cb.state,
        };
        let req = self.post(&["content_blocks", "create"]).json(&body);
        let resp: ContentBlockCreateResponse = self.send_json(req).await?;
        Ok(resp.content_block_id)
    }

    /// `POST /content_blocks/update` — overwrite an existing content block.
    /// Used both for body changes and for the `--archive-orphans` rename
    /// (which is "update with the same id but a `[ARCHIVED-...]` name").
    pub async fn update_content_block(
        &self,
        id: &str,
        cb: &ContentBlock,
    ) -> Result<(), BrazeApiError> {
        let body = ContentBlockWriteBody {
            content_block_id: Some(id),
            name: &cb.name,
            description: cb.description.as_deref(),
            content: &cb.content,
            tags: &cb.tags,
            state: cb.state,
        };
        let req = self.post(&["content_blocks", "update"]).json(&body);
        self.send_ok(req).await
    }
}

// =====================================================================
// Wire types — ASSUMED, pinned by tests below.
// =====================================================================

#[derive(Debug, Deserialize)]
struct ContentBlockListResponse {
    #[serde(default)]
    content_blocks: Vec<ContentBlockListEntry>,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ContentBlockListEntry {
    content_block_id: String,
    name: String,
}

#[derive(Debug, Deserialize)]
struct ContentBlockInfoResponse {
    #[serde(default)]
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    message: Option<String>,
}

impl ContentBlockInfoResponse {
    fn is_success(&self) -> bool {
        match &self.message {
            None => true,
            Some(m) => m.eq_ignore_ascii_case("success"),
        }
    }
}

#[derive(Serialize)]
struct ContentBlockWriteBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    content_block_id: Option<&'a str>,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    content: &'a str,
    tags: &'a [String],
    state: ContentBlockState,
}

#[derive(Debug, Deserialize)]
struct ContentBlockCreateResponse {
    content_block_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::StatusCode;
    use secrecy::SecretString;
    use serde_json::json;
    use url::Url;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_client(server: &MockServer) -> BrazeClient {
        BrazeClient::new(
            Url::parse(&server.uri()).unwrap(),
            SecretString::from("test-key".to_string()),
            10_000,
        )
    }

    #[tokio::test]
    async fn list_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .and(header("authorization", "Bearer test-key"))
            .and(query_param("limit", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2,
                "content_blocks": [
                    {"content_block_id": "id-1", "name": "promo"},
                    {"content_block_id": "id-2", "name": "header"}
                ],
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let summaries = client.list_content_blocks().await.unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].content_block_id, "id-1");
        assert_eq!(summaries[0].name, "promo");
    }

    #[tokio::test]
    async fn list_empty_array() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"content_blocks": []})))
            .mount(&server)
            .await;
        let client = make_client(&server);
        assert!(client.list_content_blocks().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_ignores_unknown_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content_blocks": [{
                    "content_block_id": "id-1",
                    "name": "promo",
                    "content_type": "html",
                    "liquid_tag": "{{content_blocks.${promo}}}",
                    "future_metadata": {"foo": "bar"}
                }]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let summaries = client.list_content_blocks().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "promo");
    }

    #[tokio::test]
    async fn list_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_content_blocks().await.unwrap_err();
        assert!(matches!(err, BrazeApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn info_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/info"))
            .and(query_param("content_block_id", "id-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content_block_id": "id-1",
                "name": "promo",
                "description": "Promo banner",
                "content": "Hello {{ user.${first_name} }}",
                "tags": ["pr", "dialog"],
                "content_type": "html",
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let cb = client.get_content_block("id-1").await.unwrap();
        assert_eq!(cb.name, "promo");
        assert_eq!(cb.description.as_deref(), Some("Promo banner"));
        assert_eq!(cb.content, "Hello {{ user.${first_name} }}");
        assert_eq!(cb.tags, vec!["pr".to_string(), "dialog".to_string()]);
        // Braze does not return state; we default to Active.
        assert_eq!(cb.state, ContentBlockState::Active);
    }

    #[tokio::test]
    async fn info_with_unsuccessful_message_is_not_found() {
        let server = MockServer::start().await;
        // Braze sometimes returns 200 with `"message": "..."` carrying
        // the failure reason instead of a 4xx. Treat any non-"success"
        // message as NotFound for the get-by-id case.
        Mock::given(method("GET"))
            .and(path("/content_blocks/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": "No content block with id 'missing' found"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.get_content_block("missing").await.unwrap_err();
        match err {
            BrazeApiError::NotFound { resource } => assert!(resource.contains("missing")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_sends_correct_body_and_returns_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/content_blocks/create"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(json!({
                "name": "promo",
                "description": "Promo banner",
                "content": "Hello",
                "tags": ["pr"],
                "state": "active"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content_block_id": "new-id-123",
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let cb = ContentBlock {
            name: "promo".into(),
            description: Some("Promo banner".into()),
            content: "Hello".into(),
            tags: vec!["pr".into()],
            state: ContentBlockState::Active,
        };
        let id = client.create_content_block(&cb).await.unwrap();
        assert_eq!(id, "new-id-123");
    }

    #[tokio::test]
    async fn create_omits_description_when_none() {
        // Test that the optional description field doesn't appear in
        // the wire body when it's None — confirms #[serde(skip_serializing_if)].
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/content_blocks/create"))
            .and(body_json(json!({
                "name": "minimal",
                "content": "x",
                "tags": [],
                "state": "active"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content_block_id": "id-min"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cb = ContentBlock {
            name: "minimal".into(),
            description: None,
            content: "x".into(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        client.create_content_block(&cb).await.unwrap();
    }

    #[tokio::test]
    async fn update_sends_id_in_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/content_blocks/update"))
            .and(body_json(json!({
                "content_block_id": "id-1",
                "name": "promo",
                "content": "Updated body",
                "tags": [],
                "state": "active"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let cb = ContentBlock {
            name: "promo".into(),
            description: None,
            content: "Updated body".into(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        client.update_content_block("id-1", &cb).await.unwrap();
    }

    #[tokio::test]
    async fn update_unauthorized_propagates() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/content_blocks/update"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cb = ContentBlock {
            name: "x".into(),
            description: None,
            content: String::new(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        let err = client.update_content_block("id", &cb).await.unwrap_err();
        assert!(matches!(err, BrazeApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn update_server_error_is_http() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/content_blocks/update"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cb = ContentBlock {
            name: "x".into(),
            description: None,
            content: String::new(),
            tags: vec![],
            state: ContentBlockState::Active,
        };
        let err = client.update_content_block("id", &cb).await.unwrap_err();
        match err {
            BrazeApiError::Http { status, body } => {
                assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
                assert!(body.contains("oops"));
            }
            other => panic!("expected Http, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_retries_on_429_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content_blocks": [{"content_block_id": "id-x", "name": "x"}]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let client = make_client(&server);
        let summaries = client.list_content_blocks().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "x");
    }
}
