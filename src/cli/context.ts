import type { Command } from "commander";
import { BrazeClient } from "../core/braze-client.js";
import { loadConfig, resolveApiKey } from "../core/config.js";
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
		console.error(`Error: Environment '${envName}' not found in config`);
		process.exit(1);
	}

	const apiKey = resolveApiKey(env.api_key_env);
	const client = new BrazeClient({ apiUrl: env.api_url, apiKey, verbose });

	return { config, env, client, verbose };
}

export function getResourceTypes(config: Config, resourceOption?: string): string[] {
	if (resourceOption) {
		return [resourceOption];
	}
	return Object.keys(config.resources).filter((k) => config.resources[k as keyof ResourcePaths]);
}
