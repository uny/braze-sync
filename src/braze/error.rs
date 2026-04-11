//! Braze API error variants. See IMPLEMENTATION.md §6.7 / §8.

use reqwest::StatusCode;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum BrazeApiError {
    /// Non-success HTTP status that wasn't already mapped to a typed
    /// variant below. Carries the body so users have something to grep.
    #[error("HTTP {status}: {body}")]
    Http { status: StatusCode, body: String },

    /// Network / decode errors from `reqwest`. Includes JSON parse errors
    /// (reqwest::Error::is_decode) — the message will say so.
    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    /// 401 from Braze. Almost always a wrong / missing API key.
    #[error("authentication failed (invalid api key)")]
    Unauthorized,

    /// 404 mapped from a get-by-name endpoint. Generic Http {404} from a
    /// list endpoint stays as Http; this variant is reserved for the
    /// "this specific resource doesn't exist" case so callers can branch
    /// on it without string matching.
    #[error("Braze resource not found: {resource}")]
    NotFound { resource: String },

    /// MAX_RETRIES of 429 in a row. The caller should slow down (or
    /// raise the configured rate limit) rather than just retrying again.
    #[error("rate limit retries exhausted")]
    RateLimitExhausted,

    /// A list endpoint returned a truncated page and v0.2.0 does not yet
    /// implement pagination. Returned instead of silently dropping the
    /// missing results, because for content blocks that drop would let
    /// `apply` create duplicates of blocks living on page 2+ (and
    /// `--archive-orphans` would miss them entirely).
    #[error(
        "Braze {endpoint}: pagination not implemented in v0.2.0 ({detail}); \
         aborting to prevent duplicate-create or silent orphan loss"
    )]
    PaginationNotImplemented {
        endpoint: &'static str,
        detail: String,
    },

    /// Braze returned HTTP 200 with a non-success `message` field that
    /// does not match any known not-found phrase. Surfaced verbatim so
    /// an unexpected server-side failure is loud instead of being
    /// silently misclassified as `NotFound` — the wire shapes in v0.2.0
    /// are ASSUMED per IMPLEMENTATION.md §8.3, so any unrecognised
    /// status message is exactly the signal the operator needs to see.
    #[error("Braze {endpoint}: unexpected message in 200 response: {message}")]
    UnexpectedApiMessage {
        endpoint: &'static str,
        message: String,
    },

    /// A list endpoint returned two entries sharing the same `name`.
    /// Braze is expected to enforce name uniqueness for named resources
    /// (content blocks, email templates), so this is a contract
    /// violation rather than an operator-fixable condition. We surface
    /// it loudly because the diff/apply path indexes by name — silently
    /// keeping only one of a duplicate pair would hide a resource from
    /// every subsequent list/update/archive operation.
    #[error("Braze {endpoint}: duplicate name {name:?} in list response")]
    DuplicateNameInListResponse {
        endpoint: &'static str,
        name: String,
    },
}
