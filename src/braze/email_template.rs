//! Email Template endpoints.
//!
//! Braze identifies email templates by `email_template_id` but uses
//! `template_name` as the human-readable identifier. There is no DELETE
//! endpoint, which is why remote-only templates are surfaced as orphans
//! rather than `Removed` diffs — same pattern as Content Block (§11.6).
//!
//! API verification (2026-04-12):
//! - Field name mapping: `template_name`↔`name`, `body`↔`body_html`,
//!   `plaintext_body`↔`body_plaintext`
//! - `description` is returned by /info but NOT settable via create/update
//! - `from_address`/`from_display_name`/`reply_to` do NOT exist in the API
//! - Pagination: `limit` (default 100, max 1000) + `offset`

use crate::braze::error::BrazeApiError;
use crate::braze::{classify_info_message, BrazeClient, InfoMessageClass};
use crate::resource::EmailTemplate;
use serde::{Deserialize, Serialize};

const LIST_LIMIT: u32 = 100;

#[derive(Debug, Clone, PartialEq)]
pub struct EmailTemplateSummary {
    pub email_template_id: String,
    pub name: String,
}

impl BrazeClient {
    /// Returns one page of up to [`LIST_LIMIT`] summaries. Hard-errors
    /// rather than silently truncating: see `PaginationNotImplemented`.
    pub async fn list_email_templates(&self) -> Result<Vec<EmailTemplateSummary>, BrazeApiError> {
        let req = self
            .get(&["templates", "email", "list"])
            .query(&[("limit", LIST_LIMIT.to_string())]);
        let resp: EmailTemplateListResponse = self.send_json(req).await?;
        let returned = resp.templates.len();

        // Fail closed when the page is or might be truncated.
        // Same pattern as content_block::list_content_blocks.
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
                endpoint: "/templates/email/list",
                detail,
            });
        }

        // Duplicate names would collapse the name→id index in
        // compute_email_template_plan.
        let mut seen: std::collections::HashSet<&str> =
            std::collections::HashSet::with_capacity(resp.templates.len());
        for entry in &resp.templates {
            if !seen.insert(entry.template_name.as_str()) {
                return Err(BrazeApiError::DuplicateNameInListResponse {
                    endpoint: "/templates/email/list",
                    name: entry.template_name.clone(),
                });
            }
        }

        Ok(resp
            .templates
            .into_iter()
            .map(|w| EmailTemplateSummary {
                email_template_id: w.email_template_id,
                name: w.template_name,
            })
            .collect())
    }

    /// Fetch full template details by id. Uses the same 200+message
    /// classifier pattern as content_block::get_content_block.
    pub async fn get_email_template(&self, id: &str) -> Result<EmailTemplate, BrazeApiError> {
        let req = self
            .get(&["templates", "email", "info"])
            .query(&[("email_template_id", id)]);
        let wire: EmailTemplateInfoResponse = self.send_json(req).await?;
        match classify_info_message(wire.message.as_deref(), "no email template") {
            InfoMessageClass::Success => {}
            InfoMessageClass::NotFound => {
                return Err(BrazeApiError::NotFound {
                    resource: format!("email_template id '{id}'"),
                });
            }
            InfoMessageClass::Unexpected(message) => {
                return Err(BrazeApiError::UnexpectedApiMessage {
                    endpoint: "/templates/email/info",
                    message,
                });
            }
        }
        Ok(EmailTemplate {
            name: wire.template_name,
            subject: wire.subject.unwrap_or_default(),
            body_html: wire.body.unwrap_or_default(),
            body_plaintext: wire.plaintext_body.unwrap_or_default(),
            // description is read-only — returned by /info but not
            // settable via create/update.
            description: wire.description,
            preheader: wire.preheader,
            should_inline_css: wire.should_inline_css,
            tags: wire.tags.unwrap_or_default(),
        })
    }

    pub async fn create_email_template(&self, et: &EmailTemplate) -> Result<String, BrazeApiError> {
        let body = EmailTemplateWriteBody {
            email_template_id: None,
            template_name: &et.name,
            subject: &et.subject,
            body: &et.body_html,
            plaintext_body: Some(&et.body_plaintext),
            preheader: et.preheader.as_deref(),
            should_inline_css: et.should_inline_css,
            tags: &et.tags,
        };
        let req = self.post(&["templates", "email", "create"]).json(&body);
        let resp: EmailTemplateCreateResponse = self.send_json(req).await?;
        Ok(resp.email_template_id)
    }

    /// Update an existing email template. `description` is intentionally
    /// omitted — Braze /info returns it but create/update cannot set it.
    pub async fn update_email_template(
        &self,
        id: &str,
        et: &EmailTemplate,
    ) -> Result<(), BrazeApiError> {
        let body = EmailTemplateWriteBody {
            email_template_id: Some(id),
            template_name: &et.name,
            subject: &et.subject,
            body: &et.body_html,
            plaintext_body: Some(&et.body_plaintext),
            preheader: et.preheader.as_deref(),
            should_inline_css: et.should_inline_css,
            tags: &et.tags,
        };
        let req = self.post(&["templates", "email", "update"]).json(&body);
        self.send_ok(req).await
    }
}

// ===================================================================
// Wire types
// ===================================================================

#[derive(Debug, Deserialize)]
struct EmailTemplateListResponse {
    #[serde(default)]
    templates: Vec<EmailTemplateListEntry>,
    #[serde(default)]
    count: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct EmailTemplateListEntry {
    email_template_id: String,
    template_name: String,
}

#[derive(Debug, Deserialize)]
struct EmailTemplateInfoResponse {
    #[serde(default)]
    template_name: String,
    #[serde(default)]
    subject: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    plaintext_body: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    preheader: Option<String>,
    #[serde(default)]
    should_inline_css: Option<bool>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    message: Option<String>,
}

/// Wire body shared by create and update. `description` is intentionally
/// absent — Braze /info returns it but create/update cannot set it.
#[derive(Serialize)]
struct EmailTemplateWriteBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    email_template_id: Option<&'a str>,
    template_name: &'a str,
    subject: &'a str,
    body: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    plaintext_body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preheader: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    should_inline_css: Option<bool>,
    tags: &'a [String],
}

#[derive(Debug, Deserialize)]
struct EmailTemplateCreateResponse {
    email_template_id: String,
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
            .and(path("/templates/email/list"))
            .and(header("authorization", "Bearer test-key"))
            .and(query_param("limit", "100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2,
                "templates": [
                    {"email_template_id": "id-1", "template_name": "welcome"},
                    {"email_template_id": "id-2", "template_name": "password_reset"}
                ],
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let summaries = client.list_email_templates().await.unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].email_template_id, "id-1");
        assert_eq!(summaries[0].name, "welcome");
    }

    #[tokio::test]
    async fn list_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"templates": []})))
            .mount(&server)
            .await;
        let client = make_client(&server);
        assert!(client.list_email_templates().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_ignores_unknown_fields() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "templates": [{
                    "email_template_id": "id-1",
                    "template_name": "welcome",
                    "updated_at": "2026-04-12T00:00:00Z",
                    "created_at": "2026-01-01T00:00:00Z",
                    "tags": ["onboarding"],
                    "future_field": true
                }]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let summaries = client.list_email_templates().await.unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "welcome");
    }

    #[tokio::test]
    async fn list_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/list"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_email_templates().await.unwrap_err();
        assert!(matches!(err, BrazeApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn info_happy_path() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/info"))
            .and(query_param("email_template_id", "id-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "email_template_id": "id-1",
                "template_name": "welcome",
                "description": "Welcome email",
                "subject": "Welcome to our service",
                "body": "<p>Hello</p>",
                "plaintext_body": "Hello",
                "preheader": "Get started",
                "should_inline_css": true,
                "tags": ["onboarding", "email"],
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-04-12T00:00:00Z",
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let et = client.get_email_template("id-1").await.unwrap();
        assert_eq!(et.name, "welcome");
        assert_eq!(et.subject, "Welcome to our service");
        assert_eq!(et.body_html, "<p>Hello</p>");
        assert_eq!(et.body_plaintext, "Hello");
        assert_eq!(et.description.as_deref(), Some("Welcome email"));
        assert_eq!(et.preheader.as_deref(), Some("Get started"));
        assert_eq!(et.should_inline_css, Some(true));
        assert_eq!(et.tags, vec!["onboarding", "email"]);
    }

    #[tokio::test]
    async fn info_missing_optional_fields_default() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "template_name": "minimal",
                "message": "success"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let et = client.get_email_template("id-x").await.unwrap();
        assert_eq!(et.name, "minimal");
        assert_eq!(et.subject, "");
        assert_eq!(et.body_html, "");
        assert_eq!(et.body_plaintext, "");
        assert!(et.description.is_none());
        assert!(et.preheader.is_none());
        assert!(et.should_inline_css.is_none());
        assert!(et.tags.is_empty());
    }

    #[tokio::test]
    async fn info_not_found_message() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": "No email template with id 'missing' found"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.get_email_template("missing").await.unwrap_err();
        match err {
            BrazeApiError::NotFound { resource } => assert!(resource.contains("missing")),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn info_unexpected_message_surfaces_verbatim() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "message": "Internal server hiccup, please retry"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.get_email_template("some-id").await.unwrap_err();
        match err {
            BrazeApiError::UnexpectedApiMessage { endpoint, message } => {
                assert_eq!(endpoint, "/templates/email/info");
                assert!(message.contains("Internal server hiccup"));
            }
            other => panic!("expected UnexpectedApiMessage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_sends_correct_body_and_returns_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/templates/email/create"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(json!({
                "template_name": "welcome",
                "subject": "Welcome",
                "body": "<p>Hi</p>",
                "plaintext_body": "Hi",
                "preheader": "Get started",
                "should_inline_css": true,
                "tags": ["onboarding"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "email_template_id": "new-id-123",
                "message": "success"
            })))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let et = EmailTemplate {
            name: "welcome".into(),
            subject: "Welcome".into(),
            body_html: "<p>Hi</p>".into(),
            body_plaintext: "Hi".into(),
            description: Some("should not be sent".into()),
            preheader: Some("Get started".into()),
            should_inline_css: Some(true),
            tags: vec!["onboarding".into()],
        };
        let id = client.create_email_template(&et).await.unwrap();
        assert_eq!(id, "new-id-123");
    }

    #[tokio::test]
    async fn create_minimal_omits_optional_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/templates/email/create"))
            .and(body_json(json!({
                "template_name": "minimal",
                "subject": "x",
                "body": "",
                "plaintext_body": "",
                "tags": []
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "email_template_id": "id-min"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let et = EmailTemplate {
            name: "minimal".into(),
            subject: "x".into(),
            body_html: String::new(),
            body_plaintext: String::new(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: vec![],
        };
        client.create_email_template(&et).await.unwrap();
    }

    #[tokio::test]
    async fn update_sends_id_and_omits_description() {
        // Pins that description is NOT sent — it's read-only.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/templates/email/update"))
            .and(body_json(json!({
                "email_template_id": "id-1",
                "template_name": "welcome",
                "subject": "Updated",
                "body": "<p>New</p>",
                "plaintext_body": "New",
                "tags": []
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"message": "success"})))
            .mount(&server)
            .await;

        let client = make_client(&server);
        let et = EmailTemplate {
            name: "welcome".into(),
            subject: "Updated".into(),
            body_html: "<p>New</p>".into(),
            body_plaintext: "New".into(),
            description: Some("this should not appear in wire body".into()),
            preheader: None,
            should_inline_css: None,
            tags: vec![],
        };
        client.update_email_template("id-1", &et).await.unwrap();
    }

    #[tokio::test]
    async fn update_unauthorized_propagates() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/templates/email/update"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid"))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let et = EmailTemplate {
            name: "x".into(),
            subject: "x".into(),
            body_html: String::new(),
            body_plaintext: String::new(),
            description: None,
            preheader: None,
            should_inline_css: None,
            tags: vec![],
        };
        let err = client.update_email_template("id", &et).await.unwrap_err();
        assert!(matches!(err, BrazeApiError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn list_errors_when_count_exceeds_returned() {
        let server = MockServer::start().await;
        let entries: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                json!({
                    "email_template_id": format!("id-{i}"),
                    "template_name": format!("tpl-{i}")
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/templates/email/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 250,
                "templates": entries,
                "message": "success"
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_email_templates().await.unwrap_err();
        match err {
            BrazeApiError::PaginationNotImplemented { endpoint, detail } => {
                assert_eq!(endpoint, "/templates/email/list");
                assert!(detail.contains("100"), "detail: {detail}");
                assert!(detail.contains("250"), "detail: {detail}");
            }
            other => panic!("expected PaginationNotImplemented, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_errors_on_full_page_with_no_count_field() {
        let server = MockServer::start().await;
        let entries: Vec<serde_json::Value> = (0..100)
            .map(|i| {
                json!({
                    "email_template_id": format!("id-{i}"),
                    "template_name": format!("tpl-{i}")
                })
            })
            .collect();
        Mock::given(method("GET"))
            .and(path("/templates/email/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "templates": entries })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_email_templates().await.unwrap_err();
        assert!(
            matches!(err, BrazeApiError::PaginationNotImplemented { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn list_errors_on_duplicate_name() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/templates/email/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "count": 2,
                "templates": [
                    {"email_template_id": "id-a", "template_name": "dup"},
                    {"email_template_id": "id-b", "template_name": "dup"}
                ]
            })))
            .mount(&server)
            .await;
        let client = make_client(&server);
        let err = client.list_email_templates().await.unwrap_err();
        match err {
            BrazeApiError::DuplicateNameInListResponse { endpoint, name } => {
                assert_eq!(endpoint, "/templates/email/list");
                assert_eq!(name, "dup");
            }
            other => panic!("expected DuplicateNameInListResponse, got {other:?}"),
        }
    }
}
