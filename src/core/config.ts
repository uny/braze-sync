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
    if (typeof e.api_key_env !== "string" || !e.api_key_env) {
      throw new ConfigError(`Environment '${name}' must have a valid 'api_key_env'`);
    }
  }

  if (!obj.resources || typeof obj.resources !== "object") {
    throw new ConfigError("Config must have a 'resources' section");
  }

  return obj as unknown as Config;
}

export function resolveApiKey(envVarName: string): string {
  const key = process.env[envVarName];
  if (!key) {
    throw new ConfigError(`API key environment variable '${envVarName}' is not set`);
  }
  return key;
}
