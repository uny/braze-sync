import type { Command } from "commander";
import { BrazeClient } from "../../core/braze-client.js";
import { loadConfig, resolveApiKey } from "../../core/config.js";
import { formatDiffTable } from "../../formatters/table.js";
import { CatalogProvider } from "../../providers/catalog.js";
import { ContentBlockProvider } from "../../providers/content-block.js";
import type { DiffResult } from "../../types/diff.js";

export function registerDiffCommand(program: Command): void {
	program
		.command("diff")
		.description("Show differences between local definitions and Braze live state")
		.requiredOption("--env <name>", "Target environment name")
		.option("--resource <type>", "Filter by resource type")
		.option("--fail-on-drift", "Exit with code 1 if any drift detected")
		.action(async (opts) => {
			const configPath = program.opts().config ?? "braze-sync.config.yaml";
			const verbose = program.opts().verbose ?? false;

			const config = await loadConfig(configPath);
			const env = config.environments[opts.env];
			if (!env) {
				console.error(`Error: Environment '${opts.env}' not found in config`);
				process.exit(1);
			}

			const apiKey = resolveApiKey(env.api_key_env);
			const client = new BrazeClient({ apiUrl: env.api_url, apiKey, verbose });

			const allDiffs: DiffResult[] = [];
			const resourceTypes = opts.resource
				? [opts.resource]
				: Object.keys(config.resources).filter(
						(k) => config.resources[k as keyof typeof config.resources],
					);

			for (const resourceType of resourceTypes) {
				if (resourceType === "catalogs" && config.resources.catalogs) {
					const provider = new CatalogProvider();
					const [local, remote] = await Promise.all([
						provider.readLocal(config.resources.catalogs),
						provider.fetchRemote(client),
					]);
					allDiffs.push(...provider.diff(local, remote));
				}

				if (resourceType === "content_blocks" && config.resources.content_blocks) {
					const provider = new ContentBlockProvider();
					const [local, remote] = await Promise.all([
						provider.readLocal(config.resources.content_blocks),
						provider.fetchRemote(client),
					]);
					allDiffs.push(...provider.diff(local, remote));
				}
			}

			console.log(formatDiffTable(allDiffs));

			if (opts.failOnDrift && allDiffs.length > 0) {
				process.exit(1);
			}
		});
}
