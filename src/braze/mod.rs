//! Braze REST API client.
//!
//! Layered:
//! - [`rate_limit`]: token-bucket throttle (governor)
//! - [`error`]: typed [`error::BrazeApiError`] variants
//! - [`catalog`] (and, in Phase B, sibling modules per resource):
//!   per-endpoint async methods written as `impl BrazeClient { ... }`
//!   blocks
//!
//! Every request goes through [`BrazeClient::send_json`] so authentication,
//! `User-Agent`, rate limiting, and 429 retry behavior are defined exactly
//! once. See IMPLEMENTATION.md §8.

pub mod catalog;
pub mod error;
pub mod rate_limit;

use crate::braze::error::BrazeApiError;
use crate::braze::rate_limit::RateLimiter;
use reqwest::{Client, RequestBuilder, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use std::time::Duration;
use url::Url;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(2);

/// Cheap-to-clone Braze API client. Internally Arc-shares the API key,
/// the rate limiter, and `reqwest::Client`'s connection pool, so cloning
/// for a parallel batch is essentially free.
#[derive(Clone)]
pub struct BrazeClient {
    http: Client,
    base_url: Url,
    api_key: Arc<SecretString>,
    limiter: Arc<RateLimiter>,
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
    pub fn new(base_url: Url, api_key: SecretString, rpm: u32) -> Self {
        let http = Client::builder()
            .user_agent(concat!("braze-sync/", env!("CARGO_PKG_VERSION")))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("reqwest client builds with default features");
        Self {
            http,
            base_url,
            api_key: Arc::new(api_key),
            limiter: Arc::new(RateLimiter::new(rpm)),
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

    /// Pre-authenticated GET builder for the given path segments.
    pub(crate) fn get(&self, segments: &[&str]) -> RequestBuilder {
        let url = self.url_for(segments);
        self.http
            .get(url)
            .bearer_auth(self.api_key.expose_secret())
            .header(reqwest::header::ACCEPT, "application/json")
    }

    /// Send `builder`, applying rate limiting and 429 retry, and decode
    /// the JSON body as `T` on success.
    pub(crate) async fn send_json<T: serde::de::DeserializeOwned>(
        &self,
        builder: RequestBuilder,
    ) -> Result<T, BrazeApiError> {
        let mut attempt: u32 = 0;
        loop {
            self.limiter.acquire().await;
            let req = builder
                .try_clone()
                .expect("non-streaming requests are cloneable");
            let resp = req.send().await?;
            let status = resp.status();

            if status.is_success() {
                let body: T = resp.json().await?;
                return Ok(body);
            }
            match status {
                StatusCode::TOO_MANY_REQUESTS if attempt < MAX_RETRIES => {
                    let wait = parse_retry_after(&resp).unwrap_or(DEFAULT_RETRY_AFTER);
                    tracing::warn!(?wait, attempt, "429 received, backing off");
                    tokio::time::sleep(wait).await;
                    attempt += 1;
                }
                StatusCode::TOO_MANY_REQUESTS => {
                    return Err(BrazeApiError::RateLimitExhausted);
                }
                StatusCode::UNAUTHORIZED => return Err(BrazeApiError::Unauthorized),
                _ => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(BrazeApiError::Http { status, body });
                }
            }
        }
    }
}

fn parse_retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}
