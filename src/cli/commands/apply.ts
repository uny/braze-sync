import type { Command } from "commander";
import { formatApplyResults, formatDiffTable } from "../../formatters/table.js";
import { CatalogProvider } from "../../providers/catalog.js";
import { ContentBlockProvider } from "../../providers/content-block.js";
import type { ApplyResult, DiffResult } from "../../types/diff.js";
import { getResourceTypes, handleErrors, resolveContext } from "../context.js";

export function registerApplyCommand(program: Command): void {
  program
    .command("apply")
    .description("Apply local definitions to Braze (dry-run by default)")
    .requiredOption("--env <name>", "Target environment name")
    .option("--resource <type>", "Filter by resource type")
    .option("--confirm", "Actually apply changes (without this, only shows plan)")
    .option("--allow-destructive", "Allow destructive operations")
    .action(
      handleErrors(async (opts) => {
        const { config, client } = await resolveContext(program, opts.env);

        const applyOptions = {
          confirm: opts.confirm ?? false,
          allowDestructive: opts.allowDestructive ?? false,
        };

        if (!applyOptions.confirm) {
          console.log("Running in dry-run mode. Use --confirm to apply changes.\n");
        }

        const allDiffs: DiffResult[] = [];
        const allResults: ApplyResult[] = [];
        const resourceTypes = getResourceTypes(config, opts.resource);

        for (const resourceType of resourceTypes) {
          if (resourceType === "catalogs" && config.resources.catalogs) {
            const provider = new CatalogProvider();
            const [local, remote] = await Promise.all([
              provider.readLocal(config.resources.catalogs),
              provider.fetchRemote(client),
            ]);
            const diffs = provider.diff(local, remote);
            allDiffs.push(...diffs);

            if (diffs.length > 0) {
              const results = await provider.applyWithLocal(client, diffs, applyOptions, local);
              allResults.push(...results);
            }
          }

          if (resourceType === "content_blocks" && config.resources.content_blocks) {
            const provider = new ContentBlockProvider();
            const [local, remote] = await Promise.all([
              provider.readLocal(config.resources.content_blocks),
              provider.fetchRemote(client),
            ]);
            const diffs = provider.diff(local, remote);
            allDiffs.push(...diffs);

            if (diffs.length > 0) {
              const results = await provider.applyWithLocal(
                client,
                diffs,
                applyOptions,
                local,
                remote,
              );
              allResults.push(...results);
            }
          }
        }

        if (allDiffs.length === 0) {
          console.log("No differences found. Nothing to apply.");
          return;
        }

        console.log("Change plan:");
        console.log(formatDiffTable(allDiffs));
        console.log(formatApplyResults(allResults));

        const failed = allResults.filter((r) => !r.success);
        if (failed.length > 0) {
          process.exit(1);
        }
      }),
    );
}
