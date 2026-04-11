//! Content Block endpoints.
//!
//! Braze identifies content blocks by `content_block_id` but the templates
//! that consume them reference `name`, so the workflow is always
//! list-then-translate. There is no DELETE endpoint, which is why
//! remote-only blocks are surfaced as orphans rather than `Removed` diffs.

use crate::braze::error::BrazeApiError;
use crate::braze::BrazeClient;
use crate::resource::{ContentBlock, ContentBlockState};
use serde::{Deserialize, Serialize};

const LIST_LIMIT: u32 = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct ContentBlockSummary {
    pub content_block_id: String,
    pub name: String,
}

impl BrazeClient {
    /// Returns one page of up to [`LIST_LIMIT`] summaries. Hard-errors
    /// rather than silently truncating: see `PaginationNotImplemented`.
    pub async fn list_content_blocks(&self) -> Result<Vec<ContentBlockSummary>, BrazeApiError> {
        let req = self
            .get(&["content_blocks", "list"])
            .query(&[("limit", LIST_LIMIT.to_string())]);
        let resp: ContentBlockListResponse = self.send_json(req).await?;
        let returned = resp.content_blocks.len();

        // Fail closed when the page is or might be truncated. The
        // ambiguous case (full page, no `count`) is treated as truncated
        // because we'd rather refuse a workspace that happens to have
        // exactly LIST_LIMIT blocks than let apply create duplicates of
        // page-2 blocks in a workspace with LIST_LIMIT + N.
        let truncation_detail: Option<String> = match resp.count {
            Some(total) if total > returned => Some(format!("got {returned} of {total} results")),
            None if returned >= LIST_LIMIT as usize => Some(format!(
                "got a full page of {returned} result(s) with no total reported; \
                 cannot verify whether more exist"
            )),
            _ => None,
        };
        if let Some(detail) = truncation_detail {
            return Err(BrazeApiError::PaginationNotImplemented {
                endpoint: "/content_blocks/list",
                detail,
            });
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

    /// Braze returns 200 with a non-success `message` field for unknown
    /// ids instead of a 404, so we remap that case to `NotFound` to keep
    /// the call sites consistent with the catalog client.
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
            // Braze /info has no state field; default keeps round-trips
            // stable. See diff/content_block.rs syncable_eq for why this
            // can't drift the diff layer.
            state: ContentBlockState::Active,
        })
    }

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

    /// Used both for body changes and for the `--archive-orphans` rename
    /// (same id, `[ARCHIVED-...]` name).
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
    use crate::braze::test_client as make_client;
    use reqwest::StatusCode;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
    async fn list_errors_when_count_exceeds_returned() {
        let server = MockServer::start().await;
        let entries: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                json!({
                    "content_block_id": format!("id-{i}"),
                    "name": format!("block-{i}")
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 250,
                "content_blocks": entries,
                "message": "success"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_content_blocks().await.unwrap_err();
        match err {
            BrazeApiError::PaginationNotImplemented { endpoint, detail } => {
                assert_eq!(endpoint, "/content_blocks/list");
                assert!(detail.contains("100"), "detail: {detail}");
                assert!(detail.contains("250"), "detail: {detail}");
            }
            other => panic!("expected PaginationNotImplemented, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_errors_on_full_page_with_no_count_field() {
        // Ambiguous case: 100 returned, no `count` to disambiguate.
        // Fail closed rather than risk page-2 invisibility.
        let server = MockServer::start().await;
        let entries: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                json!({
                    "content_block_id": format!("id-{i}"),
                    "name": format!("block-{i}")
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "content_blocks": entries })),
            )
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_content_blocks().await.unwrap_err();
        assert!(
            matches!(err, BrazeApiError::PaginationNotImplemented { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn list_succeeds_when_count_matches_full_page_exactly() {
        // 100 returned + count: 100 → exact full workspace, definitely
        // no more pages, must succeed.
        let server = MockServer::start().await;
        let entries: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                json!({
                    "content_block_id": format!("id-{i}"),
                    "name": format!("block-{i}")
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 100,
                "content_blocks": entries
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let summaries = client.list_content_blocks().await.unwrap();
        assert_eq!(summaries.len(), 100);
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
