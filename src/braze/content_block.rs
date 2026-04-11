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
        // The remaining `_ => None` arm also covers `None if returned <
        // LIST_LIMIT`: a short page with no `count` is trusted as the
        // full workspace because every paginated API we know of returns
        // exactly `limit` when more pages exist. If Braze ever returns a
        // soft-filtered short page (e.g. tombstoned entries hidden
        // server-side), that assumption would silently truncate — worth
        // revisiting in Phase C alongside real pagination.
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

        // Duplicate names would collapse the name→id index in
        // `diff::compute_content_block_plan`, making one of a pair
        // invisible to every subsequent list/update/archive op. Braze
        // is expected to enforce uniqueness, so this is a loud contract
        // violation, not a recoverable condition.
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(resp.content_blocks.len());
        for entry in &resp.content_blocks {
            if !seen.insert(entry.name.as_str()) {
                return Err(BrazeApiError::DuplicateNameInListResponse {
                    endpoint: "/content_blocks/list",
                    name: entry.name.clone(),
                });
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

    /// Braze returns 200 with a non-success `message` field for unknown
    /// ids instead of a 404, so we need to discriminate here rather than
    /// relying on HTTP status. Recognised not-found phrases remap to
    /// `NotFound` so callers can branch cleanly; any other non-"success"
    /// message surfaces verbatim as `UnexpectedApiMessage` so a real
    /// failure is not silently swallowed. The wire shapes are ASSUMED
    /// per IMPLEMENTATION.md §8.3 — a blanket "non-success → NotFound"
    /// rule would misclassify every future surprise as a missing id.
    pub async fn get_content_block(&self, id: &str) -> Result<ContentBlock, BrazeApiError> {
        let req = self
            .get(&["content_blocks", "info"])
            .query(&[("content_block_id", id)]);
        let wire: ContentBlockInfoResponse = self.send_json(req).await?;
        match wire.classify_message() {
            InfoMessageClass::Success => {}
            InfoMessageClass::NotFound => {
                return Err(BrazeApiError::NotFound {
                    resource: format!("content_block id '{id}'"),
                });
            }
            InfoMessageClass::Unexpected(message) => {
                return Err(BrazeApiError::UnexpectedApiMessage {
                    endpoint: "/content_blocks/info",
                    message,
                });
            }
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
            // Create is the one time braze-sync communicates an initial
            // state to Braze. On update we omit state entirely — see the
            // note on update_content_block.
            state: Some(cb.state),
        };
        let req = self.post(&["content_blocks", "create"]).json(&body);
        let resp: ContentBlockCreateResponse = self.send_json(req).await?;
        Ok(resp.content_block_id)
    }

    /// Used both for body changes and for the `--archive-orphans` rename
    /// (same id, `[ARCHIVED-...]` name).
    ///
    /// `state` is intentionally omitted from the request body. The
    /// diff layer excludes it from `syncable_eq` (there is no state
    /// field on `/content_blocks/info`, so we cannot read it back and
    /// cannot compare it), and the README documents it as a local-only
    /// field. Forwarding `cb.state` here would let local edits leak
    /// into Braze piggyback-style whenever another field changed, and
    /// could silently overwrite a real remote state that braze-sync
    /// has no way to observe — the same "infinite drift" trap the
    /// honest-orphan design exists to avoid. Leaving state off makes
    /// the wire-level behavior match the documented semantics.
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
            state: None,
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

/// Outcome of classifying the `message` field on a `/content_blocks/info`
/// response. `NotFound` preserves the call-site branching contract; the
/// `Unexpected` arm exists so an unknown message does not get silently
/// folded into `NotFound` — see the doc comment on `get_content_block`.
enum InfoMessageClass {
    Success,
    NotFound,
    Unexpected(String),
}

impl ContentBlockInfoResponse {
    fn classify_message(&self) -> InfoMessageClass {
        let Some(raw) = self.message.as_deref() else {
            return InfoMessageClass::Success;
        };
        let trimmed = raw.trim();
        if trimmed.eq_ignore_ascii_case("success") {
            return InfoMessageClass::Success;
        }
        let lower = trimmed.to_ascii_lowercase();
        // Match the known not-found phrasings conservatively. Anything
        // we don't recognise must NOT be treated as NotFound — that is
        // the whole point of this classifier over the previous boolean
        // check.
        if lower.contains("not found")
            || lower.contains("no content block")
            || lower.contains("does not exist")
        {
            InfoMessageClass::NotFound
        } else {
            InfoMessageClass::Unexpected(raw.to_string())
        }
    }
}

/// Wire body shared by `/content_blocks/create` and `.../update`. Both
/// endpoints are replace-all on the fields serialized here: `tags` is
/// always sent (an empty array drops every tag server-side) and
/// `content` overwrites the current body. `description` is sent when
/// `Some` (including `Some("")` — see `diff::content_block::desc_eq`
/// for why empty-string is semantically equivalent to no description
/// at diff time but still goes over the wire if present locally).
/// `state` is the one field we intentionally do NOT round-trip on
/// update — see the doc comment on `update_content_block`.
#[derive(Serialize)]
struct ContentBlockWriteBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    content_block_id: Option<&'a str>,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    content: &'a str,
    tags: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<ContentBlockState>,
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
    async fn info_with_unrecognised_error_message_surfaces_as_unexpected() {
        // Regression guard: before this change, any non-"success"
        // message was blanket-remapped to NotFound, which would silently
        // mask a real server-side failure as a missing id. The classifier
        // now only remaps known not-found phrases, so a novel message
        // has to come back as `UnexpectedApiMessage`.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": "Internal server hiccup, please retry"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.get_content_block("some-id").await.unwrap_err();
        match err {
            BrazeApiError::UnexpectedApiMessage { endpoint, message } => {
                assert_eq!(endpoint, "/content_blocks/info");
                assert!(
                    message.contains("Internal server hiccup"),
                    "message not preserved verbatim: {message}"
                );
            }
            other => panic!("expected UnexpectedApiMessage, got {other:?}"),
        }
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
    async fn create_forwards_draft_state_to_request_body() {
        // Counterpart to `update_sends_id_in_body_and_omits_state`: on
        // create, state IS sent. The only difference between Active and
        // Draft round-trips is the body_json matcher, so pinning Draft
        // here locks in both serde variants going over the wire.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/content_blocks/create"))
            .and(body_json(json!({
                "name": "wip",
                "content": "draft body",
                "tags": [],
                "state": "draft"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content_block_id": "id-wip"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = make_client(&server);
        let cb = ContentBlock {
            name: "wip".into(),
            description: None,
            content: "draft body".into(),
            tags: vec![],
            state: ContentBlockState::Draft,
        };
        client.create_content_block(&cb).await.unwrap();
    }

    #[tokio::test]
    async fn update_sends_id_in_body_and_omits_state() {
        // Pins two invariants: the update body carries
        // `content_block_id` (so Braze knows which block to modify),
        // and it does NOT carry a `state` field. State is local-only
        // per the README and `diff::content_block::syncable_eq`;
        // sending it here would let a local `state: draft` silently
        // overwrite the remote whenever another field happened to
        // change.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/content_blocks/update"))
            .and(body_json(json!({
                "content_block_id": "id-1",
                "name": "promo",
                "content": "Updated body",
                "tags": []
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
            .mount(&server)
            .await;

        let client = make_client(&server);
        // Deliberately pick Draft here: if the client still forwarded
        // state on update, `body_json` above would fail to match
        // because the body would carry `"state": "draft"`.
        let cb = ContentBlock {
            name: "promo".into(),
            description: None,
            content: "Updated body".into(),
            tags: vec![],
            state: ContentBlockState::Draft,
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
    async fn list_short_page_with_no_count_is_trusted_as_complete() {
        // Pins the `_ => None` arm of the truncation match for the
        // non-empty-short-page-no-count case. `list_empty_array`
        // covers the 0-entry flavour; this test nails down that a
        // partial-but-under-LIMIT page without `count` is accepted
        // as the full workspace. Matches the comment on
        // `list_content_blocks` about every known paginated API
        // returning exactly `limit` when more pages exist.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content_blocks": [
                    {"content_block_id": "id-1", "name": "a"},
                    {"content_block_id": "id-2", "name": "b"}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let summaries = client.list_content_blocks().await.unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].name, "a");
        assert_eq!(summaries[1].name, "b");
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
    async fn list_errors_on_duplicate_name_in_response() {
        // Regression guard: if Braze ever violates its own name-uniqueness
        // contract, the BTreeMap-based name→id index in
        // `diff::compute_content_block_plan` would silently keep only
        // the last id for a duplicate pair, hiding one of the two blocks
        // from every subsequent list/update/archive op. Fail loud instead.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/content_blocks/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2,
                "content_blocks": [
                    {"content_block_id": "id-a", "name": "dup"},
                    {"content_block_id": "id-b", "name": "dup"}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_content_blocks().await.unwrap_err();
        match err {
            BrazeApiError::DuplicateNameInListResponse { endpoint, name } => {
                assert_eq!(endpoint, "/content_blocks/list");
                assert_eq!(name, "dup");
            }
            other => panic!("expected DuplicateNameInListResponse, got {other:?}"),
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
