import type { Command } from "commander";
import { BrazeClient } from "../core/braze-client.js";
import { ConfigError, loadConfig, resolveApiKey } from "../core/config.js";
import type { Config, Environment, ResourcePaths } from "../types/config.js";

export interface CommandContext {
  config: Config;
  env: Environment;
  client: BrazeClient;
  verbose: boolean;
}

export async function resolveContext(program: Command, envName: string): Promise<CommandContext> {
  const configPath = program.opts().config ?? "braze-sync.config.yaml";
  const verbose = program.opts().verbose ?? false;

  const config = await loadConfig(configPath);
  const env = config.environments[envName];
  if (!env) {
    throw new ConfigError(`Environment '${envName}' not found in config`);
  }

  const apiKey = resolveApiKey(env.api_key_env);
  const client = new BrazeClient({ apiUrl: env.api_url, apiKey, verbose });

  return { config, env, client, verbose };
}

const RESOURCE_TYPE_ALIASES: Record<string, keyof ResourcePaths> = {
  catalogs: "catalogs",
  catalog: "catalogs",
  content_blocks: "content_blocks",
  content_block: "content_blocks",
  custom_attributes: "custom_attributes",
  custom_attribute: "custom_attributes",
  email_templates: "email_templates",
  email_template: "email_templates",
};

export function getResourceTypes(config: Config, resourceOption?: string): string[] {
  if (resourceOption) {
    const normalized = RESOURCE_TYPE_ALIASES[resourceOption];
    if (!normalized) {
      throw new ConfigError(
        `Unknown resource type '${resourceOption}'. Valid types: ${[...new Set(Object.values(RESOURCE_TYPE_ALIASES))].join(", ")}`,
      );
    }
    return [normalized];
  }
  return Object.keys(config.resources).filter((k) => config.resources[k as keyof ResourcePaths]);
}

/**
 * Wraps a command action handler with top-level error handling.
 * Prints user-friendly error messages instead of raw stack traces.
 */
export function handleErrors<T extends unknown[]>(
  fn: (...args: T) => Promise<void>,
): (...args: T) => Promise<void> {
  return async (...args: T) => {
    try {
      await fn(...args);
    } catch (error) {
      if (error instanceof ConfigError) {
        console.error(`Error: ${error.message}`);
      } else if (error instanceof Error) {
        console.error(`Error: ${error.message}`);
        if (process.env.DEBUG) {
          console.error(error.stack);
        }
      } else {
        console.error(`Error: ${error}`);
      }
      process.exit(1);
    }
  };
}
