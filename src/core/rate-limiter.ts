export class RateLimiter {
  private tokens: number;
  private lastRefill: number;
  private readonly maxTokens: number;
  private readonly refillRate: number; // tokens per ms

  constructor(requestsPerMinute: number) {
    this.maxTokens = requestsPerMinute;
    this.tokens = requestsPerMinute;
    this.refillRate = requestsPerMinute / 60_000;
    this.lastRefill = Date.now();
  }

  private refill(): void {
    const now = Date.now();
    const elapsed = now - this.lastRefill;
    this.tokens = Math.min(this.maxTokens, this.tokens + elapsed * this.refillRate);
    this.lastRefill = now;
  }

  async acquire(): Promise<void> {
    this.refill();

    if (this.tokens >= 1) {
      this.tokens -= 1;
      return;
    }

    let waitMs = Math.ceil((1 - this.tokens) / this.refillRate);
    await sleep(waitMs);
    this.refill();

    // setTimeout precision may cause tokens to still be < 1; wait a bit more if needed
    while (this.tokens < 1) {
      waitMs = Math.ceil((1 - this.tokens) / this.refillRate);
      await sleep(Math.max(waitMs, 1));
      this.refill();
    }

    this.tokens -= 1;
  }

  get available(): number {
    this.refill();
    return Math.floor(this.tokens);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
