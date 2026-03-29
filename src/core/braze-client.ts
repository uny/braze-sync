import type {
  BrazeApiResponse,
  BrazeCatalogCreateFieldsRequest,
  BrazeCatalogCreateRequest,
  BrazeCatalogListResponse,
  BrazeContentBlockCreateRequest,
  BrazeContentBlockInfo,
  BrazeContentBlockListResponse,
  BrazeContentBlockUpdateRequest,
} from "../types/braze-api.js";
import { RateLimiter } from "./rate-limiter.js";

export class BrazeApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly body: string,
    public readonly operation: string,
  ) {
    super(`Braze API error (${operation}): HTTP ${status} — ${body}`);
    this.name = "BrazeApiError";
  }
}

export interface BrazeClientOptions {
  apiUrl: string;
  apiKey: string;
  verbose?: boolean;
}

export class BrazeClient {
  private static readonly MAX_RETRIES = 5;
  private readonly apiUrl: string;
  private readonly apiKey: string;
  private readonly verbose: boolean;
  private readonly catalogLimiter = new RateLimiter(40); // 40 req/min (safety margin from 50)

  constructor(options: BrazeClientOptions) {
    this.apiUrl = options.apiUrl.replace(/\/$/, "");
    this.apiKey = options.apiKey;
    this.verbose = options.verbose ?? false;
  }

  private log(msg: string): void {
    if (this.verbose) {
      console.error(`[braze-client] ${msg}`);
    }
  }

  private async request<T>(
    method: string,
    path: string,
    body?: unknown,
    useCatalogLimiter = false,
    retryCount = 0,
  ): Promise<T> {
    if (useCatalogLimiter) {
      await this.catalogLimiter.acquire();
    }

    const url = `${this.apiUrl}${path}`;
    this.log(`${method} ${path}`);

    const headers: Record<string, string> = {
      Authorization: `Bearer ${this.apiKey}`,
      "Content-Type": "application/json",
    };

    let response: Response;
    try {
      response = await fetch(url, {
        method,
        headers,
        body: body ? JSON.stringify(body) : undefined,
      });
    } catch (e) {
      throw new BrazeApiError(
        0,
        `Network error: ${e instanceof Error ? e.message : String(e)}`,
        `${method} ${path}`,
      );
    }

    // Handle rate limiting with retry
    if (response.status === 429) {
      if (retryCount >= BrazeClient.MAX_RETRIES) {
        const text = await response.text();
        throw new BrazeApiError(429, text, `${method} ${path} (max retries exceeded)`);
      }
      const retryAfter = response.headers.get("Retry-After");
      const parsed = retryAfter ? Number.parseInt(retryAfter, 10) : Number.NaN;
      const waitMs = Number.isNaN(parsed) ? 60_000 : Math.max(parsed * 1000, 1_000);
      this.log(
        `Rate limited. Retrying after ${waitMs}ms (attempt ${retryCount + 1}/${BrazeClient.MAX_RETRIES})`,
      );
      await new Promise((resolve) => setTimeout(resolve, waitMs));
      return this.request<T>(method, path, body, useCatalogLimiter, retryCount + 1);
    }

    const text = await response.text();

    if (!response.ok) {
      throw new BrazeApiError(response.status, text, `${method} ${path}`);
    }

    try {
      return JSON.parse(text) as T;
    } catch {
      throw new BrazeApiError(
        response.status,
        `Invalid JSON response: ${text.slice(0, 200)}`,
        `${method} ${path}`,
      );
    }
  }

  // Catalogs

  async listCatalogs(): Promise<BrazeCatalogListResponse> {
    return this.request<BrazeCatalogListResponse>("GET", "/catalogs", undefined, true);
  }

  async createCatalog(data: BrazeCatalogCreateRequest): Promise<BrazeApiResponse> {
    return this.request<BrazeApiResponse>("POST", "/catalogs", data, true);
  }

  async createCatalogFields(
    catalogName: string,
    data: BrazeCatalogCreateFieldsRequest,
  ): Promise<BrazeApiResponse> {
    return this.request<BrazeApiResponse>(
      "POST",
      `/catalogs/${encodeURIComponent(catalogName)}/fields`,
      data,
      true,
    );
  }

  async deleteCatalogField(catalogName: string, fieldName: string): Promise<BrazeApiResponse> {
    return this.request<BrazeApiResponse>(
      "DELETE",
      `/catalogs/${encodeURIComponent(catalogName)}/fields/${encodeURIComponent(fieldName)}`,
      undefined,
      true,
    );
  }

  // Content Blocks

  async listContentBlocks(limit = 1000, offset = 0): Promise<BrazeContentBlockListResponse> {
    return this.request<BrazeContentBlockListResponse>(
      "GET",
      `/content_blocks/list?limit=${limit}&offset=${offset}`,
    );
  }

  async getContentBlockInfo(contentBlockId: string): Promise<BrazeContentBlockInfo> {
    return this.request<BrazeContentBlockInfo>(
      "GET",
      `/content_blocks/info?content_block_id=${encodeURIComponent(contentBlockId)}`,
    );
  }

  async createContentBlock(
    data: BrazeContentBlockCreateRequest,
  ): Promise<BrazeApiResponse & { content_block_id: string }> {
    return this.request("POST", "/content_blocks/create", data);
  }

  async updateContentBlock(data: BrazeContentBlockUpdateRequest): Promise<BrazeApiResponse> {
    return this.request("POST", "/content_blocks/update", data);
  }
}
