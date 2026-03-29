import { type Mock, afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BrazeApiError, BrazeClient } from "../../../src/core/braze-client.js";

describe("BrazeClient", () => {
  let client: BrazeClient;
  let fetchMock: Mock;

  beforeEach(() => {
    fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    client = new BrazeClient({
      apiUrl: "https://rest.test.braze.eu",
      apiKey: "test-api-key",
    });
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  function jsonResponse(body: unknown, status = 200, headers?: Record<string, string>): Response {
    return new Response(JSON.stringify(body), {
      status,
      headers: { "Content-Type": "application/json", ...headers },
    });
  }

  describe("successful requests", () => {
    it("sends GET request with auth header", async () => {
      fetchMock.mockResolvedValueOnce(jsonResponse({ catalogs: [], message: "success" }));

      const result = await client.listCatalogs();
      expect(result.catalogs).toEqual([]);

      const [url, opts] = fetchMock.mock.calls[0];
      expect(url).toBe("https://rest.test.braze.eu/catalogs");
      expect(opts.method).toBe("GET");
      expect(opts.headers.Authorization).toBe("Bearer test-api-key");
    });

    it("sends POST request with JSON body", async () => {
      fetchMock.mockResolvedValueOnce(
        jsonResponse({ message: "success", content_block_id: "cb-123" }),
      );

      await client.createContentBlock({
        name: "test",
        content: "<div/>",
      });

      const [, opts] = fetchMock.mock.calls[0];
      expect(opts.method).toBe("POST");
      expect(opts.headers["Content-Type"]).toBe("application/json");
      const body = JSON.parse(opts.body);
      expect(body.name).toBe("test");
    });

    it("strips trailing slash from apiUrl", async () => {
      const c = new BrazeClient({
        apiUrl: "https://rest.test.braze.eu/",
        apiKey: "key",
      });
      fetchMock.mockResolvedValueOnce(jsonResponse({ catalogs: [], message: "ok" }));
      await c.listCatalogs();
      expect(fetchMock.mock.calls[0][0]).toBe("https://rest.test.braze.eu/catalogs");
    });
  });

  describe("error handling", () => {
    it("throws BrazeApiError on non-ok response", async () => {
      fetchMock.mockResolvedValueOnce(new Response('{"message":"Not found"}', { status: 404 }));

      await expect(client.listCatalogs()).rejects.toThrow(BrazeApiError);
      await expect(
        (async () => {
          fetchMock.mockResolvedValueOnce(new Response('{"message":"Not found"}', { status: 404 }));
          return client.listCatalogs();
        })(),
      ).rejects.toMatchObject({ status: 404 });
    });

    it("throws BrazeApiError on network error", async () => {
      fetchMock.mockRejectedValueOnce(new Error("getaddrinfo ENOTFOUND"));

      await expect(client.listCatalogs()).rejects.toThrow(BrazeApiError);
      await expect(
        (async () => {
          fetchMock.mockRejectedValueOnce(new Error("getaddrinfo ENOTFOUND"));
          return client.listCatalogs();
        })(),
      ).rejects.toThrow("Network error");
    });

    it("throws BrazeApiError on invalid JSON response", async () => {
      fetchMock.mockResolvedValueOnce(new Response("not json", { status: 200 }));

      await expect(client.listCatalogs()).rejects.toThrow("Invalid JSON response");
    });
  });

  describe("rate limiting (429 retry)", () => {
    it("retries on 429 with Retry-After header", async () => {
      fetchMock
        .mockResolvedValueOnce(
          new Response("rate limited", {
            status: 429,
            headers: { "Retry-After": "1" },
          }),
        )
        .mockResolvedValueOnce(jsonResponse({ catalogs: [], message: "ok" }));

      const result = await client.listCatalogs();
      expect(result.message).toBe("ok");
      expect(fetchMock).toHaveBeenCalledTimes(2);
    }, 10_000);

    it("throws after max retries on persistent 429", async () => {
      for (let i = 0; i < 6; i++) {
        fetchMock.mockResolvedValueOnce(
          new Response("rate limited", {
            status: 429,
            headers: { "Retry-After": "1" },
          }),
        );
      }

      await expect(client.listCatalogs()).rejects.toThrow("max retries exceeded");
    }, 30_000);

    it("uses 60s default wait when Retry-After header is missing", async () => {
      // Only verify it retries — we don't wait the full 60s
      // Instead, check that the first retry is attempted
      fetchMock
        .mockResolvedValueOnce(new Response("rate limited", { status: 429 }))
        .mockResolvedValueOnce(jsonResponse({ catalogs: [], message: "ok" }));

      // Mock setTimeout to speed up the test
      const origSetTimeout = globalThis.setTimeout;
      vi.stubGlobal("setTimeout", (fn: () => void, _ms?: number) => origSetTimeout(fn, 0));

      const result = await client.listCatalogs();
      expect(result.message).toBe("ok");
      expect(fetchMock).toHaveBeenCalledTimes(2);

      vi.stubGlobal("setTimeout", origSetTimeout);
    });
  });

  describe("URL encoding", () => {
    it("encodes catalog name in URL", async () => {
      fetchMock.mockResolvedValueOnce(jsonResponse({ message: "ok" }));
      await client.deleteCatalogField("my catalog", "my field");
      const url = fetchMock.mock.calls[0][0];
      expect(url).toContain("my%20catalog");
      expect(url).toContain("my%20field");
    });

    it("encodes content_block_id in URL", async () => {
      fetchMock.mockResolvedValueOnce(
        jsonResponse({
          content_block_id: "cb-1",
          name: "test",
          content: "",
          description: "",
          state: "active",
          tags: [],
          created_at: "",
          last_edited: "",
          message: "ok",
        }),
      );
      await client.getContentBlockInfo("id with spaces");
      expect(fetchMock.mock.calls[0][0]).toContain("id%20with%20spaces");
    });
  });

  describe("verbose logging", () => {
    it("logs requests when verbose is true", async () => {
      const verboseClient = new BrazeClient({
        apiUrl: "https://rest.test.braze.eu",
        apiKey: "key",
        verbose: true,
      });
      const spy = vi.spyOn(console, "error").mockImplementation(() => {});
      fetchMock.mockResolvedValueOnce(jsonResponse({ catalogs: [], message: "ok" }));

      await verboseClient.listCatalogs();
      expect(spy).toHaveBeenCalledWith(expect.stringContaining("[braze-client]"));
      spy.mockRestore();
    });

    it("does not log when verbose is false", async () => {
      const spy = vi.spyOn(console, "error").mockImplementation(() => {});
      fetchMock.mockResolvedValueOnce(jsonResponse({ catalogs: [], message: "ok" }));

      await client.listCatalogs();
      expect(spy).not.toHaveBeenCalled();
      spy.mockRestore();
    });
  });
});
