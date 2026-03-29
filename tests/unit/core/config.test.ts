import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { ConfigError, loadConfig, validateConfig } from "../../../src/core/config.js";

const fixturesDir = join(import.meta.dirname, "../../fixtures/configs");

describe("loadConfig", () => {
  it("loads a valid config file", async () => {
    const config = await loadConfig(join(fixturesDir, "valid.yaml"));
    expect(config.version).toBe(1);
    expect(config.environments.dev.api_url).toBe("https://rest.fra-02.braze.eu");
    expect(config.environments.dev.api_key_env).toBe("BRAZE_DEV_API_KEY");
  });

  it("throws on missing file", async () => {
    await expect(loadConfig("nonexistent.yaml")).rejects.toThrow(ConfigError);
  });
});

describe("validateConfig", () => {
  it("rejects non-object", () => {
    expect(() => validateConfig(null)).toThrow("Config must be a YAML object");
    expect(() => validateConfig("string")).toThrow("Config must be a YAML object");
  });

  it("rejects wrong version", () => {
    expect(() => validateConfig({ version: 2, environments: {}, resources: {} })).toThrow(
      "Unsupported config version",
    );
  });

  it("rejects missing environments", () => {
    expect(() => validateConfig({ version: 1, resources: {} })).toThrow(
      "must have an 'environments' section",
    );
  });

  it("rejects environment without api_url", () => {
    expect(() =>
      validateConfig({
        version: 1,
        environments: { dev: { api_key_env: "KEY" } },
        resources: {},
      }),
    ).toThrow("must have a valid 'api_url'");
  });

  it("rejects environment without api_key_env", () => {
    expect(() =>
      validateConfig({
        version: 1,
        environments: { dev: { api_url: "https://example.com" } },
        resources: {},
      }),
    ).toThrow("must have a valid 'api_key_env'");
  });

  it("rejects missing resources", () => {
    expect(() =>
      validateConfig({
        version: 1,
        environments: {
          dev: {
            api_url: "https://example.com",
            api_key_env: "KEY",
          },
        },
      }),
    ).toThrow("must have a 'resources' section");
  });

  it("accepts valid config", () => {
    const config = validateConfig({
      version: 1,
      environments: {
        dev: {
          api_url: "https://rest.fra-02.braze.eu",
          api_key_env: "BRAZE_DEV_API_KEY",
        },
      },
      resources: {
        catalogs: "catalogs/",
      },
    });
    expect(config.version).toBe(1);
  });
});
