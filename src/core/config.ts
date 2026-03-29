import { readFile } from "node:fs/promises";
import { parse } from "yaml";
import type { Config } from "../types/config.js";

export class ConfigError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ConfigError";
  }
}

export async function loadConfig(configPath: string): Promise<Config> {
  let raw: string;
  try {
    raw = await readFile(configPath, "utf-8");
  } catch {
    throw new ConfigError(`Config file not found: ${configPath}`);
  }

  let parsed: unknown;
  try {
    parsed = parse(raw);
  } catch (e) {
    throw new ConfigError(
      `Invalid YAML in config file: ${e instanceof Error ? e.message : String(e)}`,
    );
  }

  return validateConfig(parsed);
}

export function validateConfig(data: unknown): Config {
  if (data === null || typeof data !== "object") {
    throw new ConfigError("Config must be a YAML object");
  }

  const obj = data as Record<string, unknown>;

  if (obj.version !== 1) {
    throw new ConfigError(`Unsupported config version: ${obj.version} (expected 1)`);
  }

  if (!obj.environments || typeof obj.environments !== "object") {
    throw new ConfigError("Config must have an 'environments' section");
  }

  const envs = obj.environments as Record<string, unknown>;
  for (const [name, env] of Object.entries(envs)) {
    if (!env || typeof env !== "object") {
      throw new ConfigError(`Environment '${name}' must be an object`);
    }
    const e = env as Record<string, unknown>;
    if (typeof e.api_url !== "string" || !e.api_url) {
      throw new ConfigError(`Environment '${name}' must have a valid 'api_url'`);
    }
    if (!e.api_url.startsWith("https://")) {
      throw new ConfigError(
        `Environment '${name}' api_url must start with https:// (got '${e.api_url}')`,
      );
    }
    if (typeof e.api_key_env !== "string" || !e.api_key_env) {
      throw new ConfigError(`Environment '${name}' must have a valid 'api_key_env'`);
    }
  }

  if (!obj.resources || typeof obj.resources !== "object") {
    throw new ConfigError("Config must have a 'resources' section");
  }

  const validResourceKeys = new Set([
    "catalogs",
    "content_blocks",
    "custom_attributes",
    "email_templates",
  ]);
  const resources = obj.resources as Record<string, unknown>;
  for (const [key, value] of Object.entries(resources)) {
    if (!validResourceKeys.has(key)) {
      throw new ConfigError(`Unknown resource type '${key}'`);
    }
    if (value !== undefined && value !== null && typeof value !== "string") {
      throw new ConfigError(`Resource '${key}' must be a path string, got ${typeof value}`);
    }
  }

  return {
    version: obj.version as number,
    environments: obj.environments as Config["environments"],
    resources: obj.resources as Config["resources"],
  };
}

export function resolveApiKey(envVarName: string): string {
  const key = process.env[envVarName];
  if (!key) {
    throw new ConfigError(`API key environment variable '${envVarName}' is not set`);
  }
  return key;
}
