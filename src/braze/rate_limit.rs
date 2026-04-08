//! Token-bucket rate limiter for outbound Braze API calls.
//!
//! Wraps `governor`'s direct (non-keyed) limiter so the rest of the crate
//! doesn't need to know the generic parameters. See IMPLEMENTATION.md §8.2.

use governor::{
    clock::DefaultClock,
    state::{InMemoryState, NotKeyed},
    Quota, RateLimiter as GovernorLimiter,
};
use std::num::NonZeroU32;

pub struct RateLimiter {
    inner: GovernorLimiter<NotKeyed, InMemoryState, DefaultClock>,
}

impl RateLimiter {
    /// Construct a limiter that releases up to `per_minute` permits per
    /// minute. Zero collapses to a sensible default (40/min) so a
    /// misconfigured config can't lock the limiter at zero throughput.
    pub fn new(per_minute: u32) -> Self {
        let fallback = NonZeroU32::new(40).expect("40 != 0");
        let n = NonZeroU32::new(per_minute).unwrap_or(fallback);
        Self {
            inner: GovernorLimiter::direct(Quota::per_minute(n)),
        }
    }

    /// Block (asynchronously) until a permit is available.
    pub async fn acquire(&self) {
        self.inner.until_ready().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn first_acquire_does_not_block_with_room_in_bucket() {
        let lim = RateLimiter::new(60);
        tokio::time::timeout(Duration::from_millis(500), lim.acquire())
            .await
            .expect("first acquire should be immediate (full bucket)");
    }

    #[tokio::test]
    async fn zero_per_minute_falls_back_to_default_and_does_not_deadlock() {
        let lim = RateLimiter::new(0);
        tokio::time::timeout(Duration::from_millis(500), lim.acquire())
            .await
            .expect("zero rpm should fall back, not block forever");
    }
}
