//! Braze REST API client.
//!
//! Layered:
//! - [`error`]: typed [`error::BrazeApiError`] variants
//! - [`catalog`] (and sibling modules per resource):
//!   per-endpoint async methods written as `impl BrazeClient { ... }`
//!   blocks
//!
//! Every request goes through [`BrazeClient::send_json`] so authentication,
//! `User-Agent`, and 429 retry behavior are defined exactly once.
//!
//! ## Rate limiting philosophy
//!
//! braze-sync does **not** carry a client-side predictive rate limiter.
//! Braze is authoritative on its own quotas (and shares pools across
//! endpoints in ways the client can't know), so we react to 429 +
//! `Retry-After` instead of pre-throttling. The retry loop below is the
//! only pacing mechanism: it honors `Retry-After` exactly when present,
//! does exponential backoff with full jitter when absent, and gives up
//! when either a total-time budget or a hard attempt cap is exceeded.

pub mod catalog;
pub mod content_block;
pub mod custom_attribute;
pub mod email_template;
pub mod error;

use crate::braze::error::BrazeApiError;
use reqwest::{Client, RequestBuilder, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Cumulative sleep budget for 429 retries on a single logical request.
/// Once total backoff sleep crosses this, the request fails fast so the
/// caller can surface a user-visible error instead of hanging.
const RETRY_BUDGET: Duration = Duration::from_secs(60);

/// Hard attempt cap. Protects against a degenerate server returning
/// `Retry-After: 0` forever (which would never consume the time budget
/// because each sleep is zero).
const RETRY_MAX_ATTEMPTS: u32 = 100;

/// Exponential-backoff parameters. Used only when the 429 response has
/// no `Retry-After` header.
const BACKOFF_BASE: Duration = Duration::from_millis(500);
const BACKOFF_CAP: Duration = Duration::from_secs(10);

/// Cheap-to-clone Braze API client. Internally Arc-shares the API key
/// and `reqwest::Client`'s connection pool, so cloning for a parallel
/// batch is essentially free.
#[derive(Clone)]
pub struct BrazeClient {
    http: Client,
    base_url: Url,
    api_key: Arc<SecretString>,
}

// Hand-written Debug to be 100% certain the api key never lands in
// tracing output, even if SecretString's own Debug impl ever changes.
impl std::fmt::Debug for BrazeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrazeClient")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl BrazeClient {
    pub fn from_resolved(resolved: &crate::config::ResolvedConfig) -> Self {
        Self::new(resolved.api_endpoint.clone(), resolved.api_key.clone())
    }

    pub fn new(base_url: Url, api_key: SecretString) -> Self {
        let http = Client::builder()
            .user_agent(concat!("braze-sync/", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client builds with default features");
        Self {
            http,
            base_url,
            api_key: Arc::new(api_key),
        }
    }

    /// Build a URL by appending each `segment` to the base URL as a
    /// separately percent-encoded path segment.
    ///
    /// User-controlled segments cannot inject path traversal or query
    /// strings because the URL crate encodes `/`, `?`, `#`, and so on
    /// inside each segment. Any path that the base URL itself carried is
    /// dropped, so the layout is predictable regardless of how the user
    /// wrote `api_endpoint` in their config.
    pub(crate) fn url_for(&self, segments: &[&str]) -> Url {
        let mut url = self.base_url.clone();
        {
            let mut seg = url
                .path_segments_mut()
                .expect("base url must be hierarchical (http/https)");
            seg.clear();
            for s in segments {
                seg.push(s);
            }
        }
        url
    }

    /// Attach bearer auth + JSON Accept to a raw request builder.
    fn authed(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.bearer_auth(self.api_key.expose_secret())
            .header(reqwest::header::ACCEPT, "application/json")
    }

    /// Pre-authenticated GET builder for the given path segments.
    pub(crate) fn get(&self, segments: &[&str]) -> RequestBuilder {
        self.authed(self.http.get(self.url_for(segments)))
    }

    pub(crate) fn post(&self, segments: &[&str]) -> RequestBuilder {
        self.authed(self.http.post(self.url_for(segments)))
    }

    pub(crate) fn delete(&self, segments: &[&str]) -> RequestBuilder {
        self.authed(self.http.delete(self.url_for(segments)))
    }

    /// Pre-authenticated GET for an absolute URL that must share origin
    /// with `base_url`. Used by pagination paths that receive the URL of
    /// the next page in a `Link: rel="next"` header. Refuses cross-origin
    /// URLs so a compromised or misconfigured upstream can't redirect us
    /// to attacker-controlled hosts carrying our bearer token.
    pub(crate) fn get_absolute(&self, url: &str) -> Result<RequestBuilder, BrazeApiError> {
        let parsed = Url::parse(url).map_err(|e| BrazeApiError::Http {
            status: StatusCode::BAD_GATEWAY,
            body: format!("malformed pagination URL {url:?}: {e}"),
        })?;
        let same_origin = parsed.scheme() == self.base_url.scheme()
            && parsed.host_str() == self.base_url.host_str()
            && parsed.port_or_known_default() == self.base_url.port_or_known_default();
        if !same_origin {
            return Err(BrazeApiError::Http {
                status: StatusCode::BAD_GATEWAY,
                body: format!(
                    "refusing cross-origin pagination URL {url:?} (base is {})",
                    self.base_url
                ),
            });
        }
        Ok(self.authed(self.http.get(parsed)))
    }

    /// Execute `builder` with 429 retry, returning the raw response on
    /// success or a typed error on failure. Shared transport layer used
    /// by both [`Self::send_json`] and [`Self::send_ok`] so the retry /
    /// auth-mapping policy lives in exactly one place.
    ///
    /// Retry policy: honor `Retry-After` (integer seconds or HTTP-date)
    /// when present; otherwise exponential backoff with full jitter
    /// (`BACKOFF_BASE * 2^attempt`, capped at `BACKOFF_CAP`). Give up
    /// when cumulative sleep exceeds [`RETRY_BUDGET`] or attempts exceed
    /// [`RETRY_MAX_ATTEMPTS`] (the latter only protects against servers
    /// that return `Retry-After: 0` forever).
    async fn send_with_retry(
        &self,
        builder: RequestBuilder,
    ) -> Result<reqwest::Response, BrazeApiError> {
        let mut attempt: u32 = 0;
        let mut elapsed = Duration::ZERO;
        loop {
            let req = builder
                .try_clone()
                .expect("non-streaming requests are cloneable");
            let resp = req.send().await?;
            let status = resp.status();

            if status.is_success() {
                return Ok(resp);
            }
            match status {
                StatusCode::TOO_MANY_REQUESTS => {
                    if attempt >= RETRY_MAX_ATTEMPTS || elapsed >= RETRY_BUDGET {
                        return Err(BrazeApiError::RateLimitExhausted);
                    }
                    let remaining = RETRY_BUDGET.saturating_sub(elapsed);
                    let wait = compute_backoff(&resp, attempt, remaining);
                    tracing::warn!(?wait, attempt, ?elapsed, "429 received, backing off");
                    tokio::time::sleep(wait).await;
                    elapsed = elapsed.saturating_add(wait);
                    attempt += 1;
                }
                StatusCode::UNAUTHORIZED => return Err(BrazeApiError::Unauthorized),
                _ => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(BrazeApiError::Http { status, body });
                }
            }
        }
    }

    /// Send `builder` and decode the JSON body as `T` on success.
    pub(crate) async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        builder: RequestBuilder,
    ) -> Result<T, BrazeApiError> {
        let resp = self.send_with_retry(builder).await?;
        Ok(resp.json::<T>().await?)
    }

    /// Like [`Self::send_json`] but also returns the `Link: rel="next"`
    /// URL if present — the only header paginated endpoints care about.
    /// Parsing it before body deserialization avoids cloning the full
    /// `HeaderMap` on every paginated request.
    pub(crate) async fn send_json_with_next_link<T: serde::de::DeserializeOwned>(
        &self,
        builder: RequestBuilder,
    ) -> Result<(T, Option<String>), BrazeApiError> {
        let resp = self.send_with_retry(builder).await?;
        let next = parse_next_link(resp.headers());
        let body = resp.json::<T>().await?;
        Ok((body, next))
    }

    /// Send `builder` and discard the response body. Used for endpoints
    /// whose only meaningful output is the HTTP status (POST add field,
    /// DELETE field). Drains the body so the connection can return to
    /// the reqwest pool cleanly even when the response is 204 No Content.
    pub(crate) async fn send_ok(&self, builder: RequestBuilder) -> Result<(), BrazeApiError> {
        let resp = self.send_with_retry(builder).await?;
        let _ = resp.bytes().await;
        Ok(())
    }
}

/// Parse a `Retry-After` header. Supports both integer seconds (the
/// common case for Braze) and RFC 7231 §7.1.3 HTTP-date (IMF-fixdate /
/// RFC 2822). Returns `None` if the header is missing or unparseable.
fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
    let raw = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?;
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    // HTTP-date. `parse_from_rfc2822` accepts IMF-fixdate (which is a
    // strict subset). Negative deltas (date already past) collapse to 0.
    let dt = chrono::DateTime::parse_from_rfc2822(raw).ok()?;
    let delta = dt
        .timestamp()
        .saturating_sub(chrono::Utc::now().timestamp());
    Some(Duration::from_secs(delta.max(0) as u64))
}

/// Compute the sleep duration before the next retry. Clamped to
/// `remaining_budget` so a degenerate server can't push a single sleep
/// past the retry budget.
fn compute_backoff(resp: &reqwest::Response, attempt: u32, remaining_budget: Duration) -> Duration {
    let wait = match parse_retry_after(resp) {
        Some(ra) => ra,
        None => {
            // `attempt.min(6)` keeps `1u32 << attempt` from overflowing;
            // `BACKOFF_BASE * 64 = 32s` already exceeds `BACKOFF_CAP`.
            let shifted = BACKOFF_BASE.saturating_mul(1u32 << attempt.min(6));
            let capped = shifted.min(BACKOFF_CAP);
            Duration::from_millis(fastrand::u64(0..=capped.as_millis() as u64))
        }
    };
    wait.min(remaining_budget)
}

/// Parse RFC 5988 Link header and return the URL associated with
/// `rel="next"`, if present. Braze uses this for cursor-style pagination
/// on `/custom_attributes`; the response body does not carry the cursor.
pub(crate) fn parse_next_link(headers: &reqwest::header::HeaderMap) -> Option<String> {
    // RFC 5988 allows repeated `Link` fields; `get_all` so a later
    // `rel="next"` isn't missed. Braze cursors are comma-free in practice.
    for hv in headers.get_all(reqwest::header::LINK) {
        let Ok(raw) = hv.to_str() else { continue };
        for part in raw.split(',') {
            let part = part.trim();
            let Some((url_part, params)) = part.split_once(';') else {
                continue;
            };
            let has_next = params.split(';').map(str::trim).any(|p| {
                let Some((k, v)) = p.split_once('=') else {
                    return false;
                };
                if !k.trim().eq_ignore_ascii_case("rel") {
                    return false;
                }
                // `rel` may be a space-delimited list (e.g. `"prev next"`).
                v.trim()
                    .trim_matches('"')
                    .split_ascii_whitespace()
                    .any(|tok| tok.eq_ignore_ascii_case("next"))
            });
            if !has_next {
                continue;
            }
            let url = url_part
                .trim()
                .trim_start_matches('<')
                .trim_end_matches('>');
            return Some(url.to_string());
        }
    }
    None
}

/// Check whether a list response was truncated and return a
/// `PaginationNotImplemented` error if so.  Shared by every list
/// endpoint that uses the fail-closed pagination guard.
pub(crate) fn check_pagination(
    count: Option<usize>,
    returned: usize,
    limit: usize,
    endpoint: &'static str,
) -> Result<(), BrazeApiError> {
    let truncation_detail: Option<String> = match count {
        Some(total) if total > returned => Some(format!("got {returned} of {total} results")),
        None if returned >= limit => Some(format!(
            "got a full page of {returned} result(s) with no total reported; \
             cannot verify whether more exist"
        )),
        _ => None,
    };
    if let Some(detail) = truncation_detail {
        return Err(BrazeApiError::PaginationNotImplemented { endpoint, detail });
    }
    Ok(())
}

/// Check that no two items in a list response share the same name.
/// Shared by every list endpoint that indexes resources by name.
pub(crate) fn check_duplicate_names<'a>(
    names: impl Iterator<Item = &'a str>,
    count: usize,
    endpoint: &'static str,
) -> Result<(), BrazeApiError> {
    let mut seen = std::collections::HashSet::with_capacity(count);
    for name in names {
        if !seen.insert(name) {
            return Err(BrazeApiError::DuplicateNameInListResponse {
                endpoint,
                name: name.to_string(),
            });
        }
    }
    Ok(())
}

/// Outcome of classifying the `message` field on a Braze `/info`
/// response. Shared by content_block and email_template — the only
/// difference is the resource-specific "not found" phrase.
pub(crate) enum InfoMessageClass {
    Success,
    NotFound,
    Unexpected(String),
}

/// Classify the `message` field returned by Braze `/info` endpoints.
/// `resource_phrase` is a resource-specific not-found indicator
/// (e.g. `"no content block"`, `"no email template"`).
pub(crate) fn classify_info_message(
    message: Option<&str>,
    resource_phrase: &str,
) -> InfoMessageClass {
    debug_assert!(
        resource_phrase == resource_phrase.to_ascii_lowercase(),
        "resource_phrase must be lowercase (compared against lowercased message)"
    );
    let Some(raw) = message else {
        return InfoMessageClass::Success;
    };
    let trimmed = raw.trim();
    if trimmed.eq_ignore_ascii_case("success") {
        return InfoMessageClass::Success;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("not found")
        || lower.contains(resource_phrase)
        || lower.contains("does not exist")
    {
        InfoMessageClass::NotFound
    } else {
        InfoMessageClass::Unexpected(raw.to_string())
    }
}

#[cfg(test)]
pub(crate) fn test_client(server: &wiremock::MockServer) -> BrazeClient {
    BrazeClient::new(
        Url::parse(&server.uri()).unwrap(),
        SecretString::from("test-key".to_string()),
    )
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use chrono::{Duration as ChronoDuration, Utc};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Build a reqwest Response with only the specified Retry-After header
    // set, without going through the network. Used to exercise the pure
    // parsing path of `parse_retry_after`.
    async fn response_with_retry_after(val: &str) -> reqwest::Response {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/r"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", val))
            .mount(&server)
            .await;
        reqwest::get(format!("{}/r", server.uri())).await.unwrap()
    }

    #[tokio::test]
    async fn retry_after_parses_integer_seconds() {
        let resp = response_with_retry_after("5").await;
        assert_eq!(parse_retry_after(&resp), Some(Duration::from_secs(5)));
    }

    #[tokio::test]
    async fn retry_after_parses_http_date() {
        let future = Utc::now() + ChronoDuration::seconds(10);
        // `%a, %d %b %Y %H:%M:%S GMT` is IMF-fixdate, accepted by rfc2822.
        let formatted = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let resp = response_with_retry_after(&formatted).await;
        let d = parse_retry_after(&resp).expect("should parse HTTP-date");
        // Allow ±2s for scheduler/clock drift in CI.
        assert!(
            d >= Duration::from_secs(8) && d <= Duration::from_secs(12),
            "expected ~10s, got {d:?}"
        );
    }

    #[tokio::test]
    async fn retry_after_past_http_date_clamps_to_zero() {
        let past = Utc::now() - ChronoDuration::seconds(30);
        let formatted = past.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let resp = response_with_retry_after(&formatted).await;
        assert_eq!(parse_retry_after(&resp), Some(Duration::ZERO));
    }

    #[tokio::test]
    async fn retry_after_unparseable_returns_none() {
        let resp = response_with_retry_after("not a date").await;
        assert_eq!(parse_retry_after(&resp), None);
    }

    #[tokio::test]
    async fn backoff_without_header_falls_back_to_exponential_jitter() {
        // No Retry-After → full-jitter draw in [0, cap).
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/r"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let resp = reqwest::get(format!("{}/r", server.uri())).await.unwrap();

        // attempt=0 → min(cap, base*1)=500ms → jitter in [0, 500ms].
        for _ in 0..20 {
            let w = compute_backoff(&resp, 0, Duration::from_secs(60));
            assert!(w <= Duration::from_millis(500), "attempt=0 bound: {w:?}");
        }
        // attempt=10 → saturates to cap (10s) → jitter in [0, 10s].
        for _ in 0..20 {
            let w = compute_backoff(&resp, 10, Duration::from_secs(60));
            assert!(w <= BACKOFF_CAP, "attempt=10 cap: {w:?}");
        }
    }

    #[tokio::test]
    async fn backoff_clamped_to_remaining_budget() {
        let resp = response_with_retry_after("30").await;
        // Server says 30s but only 5s budget left.
        let w = compute_backoff(&resp, 0, Duration::from_secs(5));
        assert_eq!(w, Duration::from_secs(5));
    }

    #[test]
    fn parse_next_link_single_rel() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::LINK,
            r#"<https://rest.example/custom_attributes/?cursor=abc>; rel="next""#
                .parse()
                .unwrap(),
        );
        assert_eq!(
            parse_next_link(&h),
            Some("https://rest.example/custom_attributes/?cursor=abc".to_string())
        );
    }

    #[test]
    fn parse_next_link_multiple_rels_picks_next() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::LINK,
            r#"<https://rest.example/?cursor=prev>; rel="prev", <https://rest.example/?cursor=next>; rel="next""#
                .parse()
                .unwrap(),
        );
        assert_eq!(
            parse_next_link(&h),
            Some("https://rest.example/?cursor=next".to_string())
        );
    }

    #[test]
    fn parse_next_link_absent_returns_none() {
        let h = reqwest::header::HeaderMap::new();
        assert_eq!(parse_next_link(&h), None);
    }

    #[test]
    fn parse_next_link_without_next_rel_returns_none() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::LINK,
            r#"<https://rest.example/?cursor=prev>; rel="prev""#
                .parse()
                .unwrap(),
        );
        assert_eq!(parse_next_link(&h), None);
    }

    #[test]
    fn parse_next_link_scans_multiple_link_header_fields() {
        // Some servers emit multiple `Link:` header fields rather than
        // comma-joining into one. `HeaderMap::get` only returns the first
        // value; we must iterate `get_all` so a later `rel="next"` still
        // wins.
        let mut h = reqwest::header::HeaderMap::new();
        h.append(
            reqwest::header::LINK,
            r#"<https://rest.example/?cursor=prev>; rel="prev""#
                .parse()
                .unwrap(),
        );
        h.append(
            reqwest::header::LINK,
            r#"<https://rest.example/?cursor=next>; rel="next""#
                .parse()
                .unwrap(),
        );
        assert_eq!(
            parse_next_link(&h),
            Some("https://rest.example/?cursor=next".to_string())
        );
    }

    #[test]
    fn parse_next_link_matches_space_delimited_rel_list() {
        // RFC 5988 allows a space-delimited list of rel tokens.
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::LINK,
            r#"<https://rest.example/?cursor=n>; rel="prev next""#
                .parse()
                .unwrap(),
        );
        assert_eq!(
            parse_next_link(&h),
            Some("https://rest.example/?cursor=n".to_string())
        );
    }

    #[tokio::test]
    async fn get_absolute_rejects_cross_origin() {
        let server = MockServer::start().await;
        let client = super::test_client(&server);
        let err = client
            .get_absolute("https://attacker.example/custom_attributes/?cursor=x")
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("cross-origin"), "got {msg:?}");
    }

    #[tokio::test]
    async fn get_absolute_accepts_same_origin() {
        let server = MockServer::start().await;
        let client = super::test_client(&server);
        let url = format!("{}/custom_attributes/?cursor=abc", server.uri());
        let _builder = client
            .get_absolute(&url)
            .expect("same-origin URL should be accepted");
    }

    #[tokio::test]
    async fn get_absolute_rejects_malformed_url() {
        let server = MockServer::start().await;
        let client = super::test_client(&server);
        let err = client.get_absolute("not a url").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("malformed"), "got {msg:?}");
    }

    #[tokio::test]
    async fn retries_attempt_cap_fires_on_degenerate_zero_retry_after() {
        // retry-after: 0 forever → attempt cap is the only exit.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
        let client = super::test_client(&server);
        let req = client.get(&["x"]);
        let err = client
            .send_json::<serde_json::Value>(req)
            .await
            .unwrap_err();
        assert!(
            matches!(err, BrazeApiError::RateLimitExhausted),
            "expected RateLimitExhausted, got {err:?}"
        );
    }
}
