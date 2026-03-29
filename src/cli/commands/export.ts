import { mkdir, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import type { Command } from "commander";
import { BrazeClient } from "../../core/braze-client.js";
import { loadConfig, resolveApiKey } from "../../core/config.js";
import { CatalogProvider } from "../../providers/catalog.js";
import { ContentBlockProvider } from "../../providers/content-block.js";

export function registerExportCommand(program: Command): void {
	program
		.command("export")
		.description("Export current Braze state to local files")
		.requiredOption("--env <name>", "Target environment name")
		.option("--resource <type>", "Filter by resource type")
		.option("--name <name>", "Filter by resource name (requires --resource)")
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

			const resourceTypes = opts.resource
				? [opts.resource]
				: Object.keys(config.resources).filter(
						(k) => config.resources[k as keyof typeof config.resources],
					);

			for (const resourceType of resourceTypes) {
				console.log(`Exporting ${resourceType}...`);

				if (resourceType === "catalogs" && config.resources.catalogs) {
					const provider = new CatalogProvider();
					const remote = await provider.fetchRemote(client);
					const filtered = opts.name ? remote.filter((r) => r.name === opts.name) : remote;

					const basePath = config.resources.catalogs;
					await mkdir(basePath, { recursive: true });

					for (const item of filtered) {
						const output = provider.serialize(item);
						const filePath = join(basePath, output.path);
						await writeFile(filePath, output.content, "utf-8");
						console.log(`  Wrote ${filePath}`);
					}
				}

				if (resourceType === "content_blocks" && config.resources.content_blocks) {
					const provider = new ContentBlockProvider();
					const remote = await provider.fetchRemote(client);
					const filtered = opts.name ? remote.filter((r) => r.name === opts.name) : remote;

					const basePath = config.resources.content_blocks;
					await mkdir(basePath, { recursive: true });

					for (const item of filtered) {
						const output = provider.serialize(item);
						const filePath = join(basePath, output.path);
						await writeFile(filePath, output.content, "utf-8");
						console.log(`  Wrote ${filePath}`);
					}
				}
			}

			console.log("Export complete.");
		});
}
