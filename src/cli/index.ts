import { Command } from "commander";
import { registerApplyCommand } from "./commands/apply.js";
import { registerDiffCommand } from "./commands/diff.js";
import { registerExportCommand } from "./commands/export.js";
import { registerValidateCommand } from "./commands/validate.js";

const program = new Command();

program
  .name("braze-sync")
  .description("GitOps CLI for Braze — export, diff, and apply resources from Git")
  .version("0.1.0")
  .option("-c, --config <path>", "Path to braze-sync.config.yaml", "braze-sync.config.yaml")
  .option("-e, --env <name>", "Target environment name")
  .option("--verbose", "Show debug output")
  .option("--format <type>", "Output format: table | json", "table");

registerExportCommand(program);
registerDiffCommand(program);
registerApplyCommand(program);
registerValidateCommand(program);

program.parse();
