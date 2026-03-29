import type { Command } from "commander";
import { loadConfig } from "../../core/config.js";
import { formatValidationErrors } from "../../formatters/table.js";
import { CatalogProvider } from "../../providers/catalog.js";
import { ContentBlockProvider } from "../../providers/content-block.js";
import type { ValidationError } from "../../types/diff.js";
import { handleErrors } from "../context.js";

export function registerValidateCommand(program: Command): void {
	program
		.command("validate")
		.description("Validate local definition files without contacting Braze API")
		.action(
			handleErrors(async () => {
				const configPath = program.opts().config ?? "braze-sync.config.yaml";
				const config = await loadConfig(configPath);
				const allErrors: ValidationError[] = [];

				if (config.resources.catalogs) {
					const provider = new CatalogProvider();
					try {
						const local = await provider.readLocal(config.resources.catalogs);
						allErrors.push(...provider.validate(local));
					} catch (e) {
						allErrors.push({
							file: config.resources.catalogs,
							message: e instanceof Error ? e.message : String(e),
						});
					}
				}

				if (config.resources.content_blocks) {
					const provider = new ContentBlockProvider();
					try {
						const local = await provider.readLocal(config.resources.content_blocks);
						allErrors.push(...provider.validate(local));
					} catch (e) {
						allErrors.push({
							file: config.resources.content_blocks,
							message: e instanceof Error ? e.message : String(e),
						});
					}
				}

				console.log(formatValidationErrors(allErrors));

				if (allErrors.length > 0) {
					process.exit(1);
				}
			}),
		);
}
