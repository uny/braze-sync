import { createRequire } from "node:module";
import { Command } from "commander";
import { registerApplyCommand } from "./commands/apply.js";
import { registerDiffCommand } from "./commands/diff.js";
import { registerExportCommand } from "./commands/export.js";
import { registerValidateCommand } from "./commands/validate.js";

const require = createRequire(import.meta.url);
const { version } = require("../../package.json") as { version: string };

const program = new Command();

program
  .name("braze-sync")
  .description("GitOps CLI for Braze — export, diff, and apply resources from Git")
  .version(version)
  .option("-c, --config <path>", "Path to braze-sync.config.yaml", "braze-sync.config.yaml")
  .option("-e, --env <name>", "Target environment name")
  .option("--verbose", "Show debug output");

registerExportCommand(program);
registerDiffCommand(program);
registerApplyCommand(program);
registerValidateCommand(program);

program.parse();
