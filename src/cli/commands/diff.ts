import type { Command } from "commander";
import { formatDiffTable } from "../../formatters/table.js";
import { CatalogProvider } from "../../providers/catalog.js";
import { ContentBlockProvider } from "../../providers/content-block.js";
import type { DiffResult } from "../../types/diff.js";
import { getResourceTypes, handleErrors, resolveContext } from "../context.js";

export function registerDiffCommand(program: Command): void {
	program
		.command("diff")
		.description("Show differences between local definitions and Braze live state")
		.requiredOption("--env <name>", "Target environment name")
		.option("--resource <type>", "Filter by resource type")
		.option("--fail-on-drift", "Exit with code 1 if any drift detected")
		.action(
			handleErrors(async (opts) => {
				const { config, client } = await resolveContext(program, opts.env);
				const allDiffs: DiffResult[] = [];
				const resourceTypes = getResourceTypes(config, opts.resource);

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
			}),
		);
}
