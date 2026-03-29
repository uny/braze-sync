import { mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import type { Command } from "commander";
import { CatalogProvider } from "../../providers/catalog.js";
import { ContentBlockProvider } from "../../providers/content-block.js";
import { getResourceTypes, handleErrors, resolveContext } from "../context.js";

function safePath(basePath: string, relativePath: string): string {
  const resolved = resolve(basePath, relativePath);
  const resolvedBase = resolve(basePath);
  if (!resolved.startsWith(`${resolvedBase}/`) && resolved !== resolvedBase) {
    throw new Error(`Path traversal detected: '${relativePath}' escapes base directory`);
  }
  return resolved;
}

export function registerExportCommand(program: Command): void {
  program
    .command("export")
    .description("Export current Braze state to local files")
    .requiredOption("--env <name>", "Target environment name")
    .option("--resource <type>", "Filter by resource type")
    .option("--name <name>", "Filter by resource name (requires --resource)")
    .action(
      handleErrors(async (opts) => {
        const { config, client } = await resolveContext(program, opts.env);
        const resourceTypes = getResourceTypes(config, opts.resource);

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
              const filePath = safePath(basePath, output.path);
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
              const filePath = safePath(basePath, output.path);
              await writeFile(filePath, output.content, "utf-8");
              console.log(`  Wrote ${filePath}`);
            }
          }
        }

        console.log("Export complete.");
      }),
    );
}
