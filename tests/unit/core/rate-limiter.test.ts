import { describe, expect, it } from "vitest";
import { RateLimiter } from "../../../src/core/rate-limiter.js";

describe("RateLimiter", () => {
  it("allows requests up to the limit", async () => {
    const limiter = new RateLimiter(10);
    expect(limiter.available).toBe(10);

    await limiter.acquire();
    expect(limiter.available).toBe(9);
  });

  it("allows burst of requests equal to max tokens", async () => {
    const limiter = new RateLimiter(5);
    for (let i = 0; i < 5; i++) {
      await limiter.acquire();
    }
    // After 5 requests, should have ~0 tokens (some refill from elapsed time)
    expect(limiter.available).toBeLessThanOrEqual(1);
  });

  it("refills tokens over time", async () => {
    const limiter = new RateLimiter(60); // 1 per second
    // Drain all tokens
    for (let i = 0; i < 60; i++) {
      await limiter.acquire();
    }
    expect(limiter.available).toBeLessThanOrEqual(1);

    // Wait 1 second → should have ~1 token
    await new Promise((resolve) => setTimeout(resolve, 1050));
    expect(limiter.available).toBeGreaterThanOrEqual(1);
  });
});
