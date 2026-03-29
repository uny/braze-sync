import { operationSymbol } from "../core/diff-engine.js";
import type { ApplyResult, DiffResult } from "../types/diff.js";
import type { ValidationError } from "../types/diff.js";

export function formatDiffTable(diffs: DiffResult[]): string {
	if (diffs.length === 0) {
		return "No differences found. Local and remote are in sync.";
	}

	const lines: string[] = [];
	lines.push("");
	lines.push(`  ${"Resource".padEnd(20)} ${"Name".padEnd(30)} ${"Op".padEnd(4)} Details`);
	lines.push(`  ${"─".repeat(20)} ${"─".repeat(30)} ${"─".repeat(4)} ${"─".repeat(40)}`);

	for (const diff of diffs) {
		const sym = operationSymbol(diff.operation);
		if (diff.details.length === 0) {
			lines.push(`  ${diff.resourceType.padEnd(20)} ${diff.resourceName.padEnd(30)} [${sym}]`);
		} else {
			lines.push(`  ${diff.resourceType.padEnd(20)} ${diff.resourceName.padEnd(30)} [${sym}]`);
			for (const detail of diff.details) {
				const detailSym = operationSymbol(detail.operation);
				let desc = `[${detailSym}] ${detail.field}`;
				if (detail.operation === "change") {
					desc += `: ${formatValue(detail.remoteValue)} → ${formatValue(detail.localValue)}`;
				} else if (detail.operation === "add") {
					desc += `: ${formatValue(detail.localValue)}`;
				} else if (detail.operation === "remove") {
					desc += `: ${formatValue(detail.remoteValue)}`;
				}
				lines.push(`  ${"".padEnd(20)} ${"".padEnd(30)}      ${desc}`);
			}
		}
	}

	lines.push("");
	const adds = diffs.filter((d) => d.operation === "add").length;
	const changes = diffs.filter((d) => d.operation === "change").length;
	const removes = diffs.filter((d) => d.operation === "remove").length;
	lines.push(`  Summary: ${adds} to add, ${changes} to change, ${removes} to remove`);
	lines.push("");

	return lines.join("\n");
}

export function formatApplyResults(results: ApplyResult[]): string {
	if (results.length === 0) {
		return "No changes to apply.";
	}

	const lines: string[] = [];
	lines.push("");
	lines.push(`  ${"Resource".padEnd(20)} ${"Name".padEnd(30)} ${"Status".padEnd(8)} Message`);
	lines.push(`  ${"─".repeat(20)} ${"─".repeat(30)} ${"─".repeat(8)} ${"─".repeat(40)}`);

	for (const result of results) {
		const status = result.success ? "OK" : "FAIL";
		lines.push(
			`  ${result.resourceType.padEnd(20)} ${result.resourceName.padEnd(30)} ${status.padEnd(8)} ${result.message}`,
		);
	}

	lines.push("");
	const succeeded = results.filter((r) => r.success).length;
	const failed = results.filter((r) => !r.success).length;
	lines.push(`  Results: ${succeeded} succeeded, ${failed} failed`);
	lines.push("");

	return lines.join("\n");
}

export function formatValidationErrors(errors: ValidationError[]): string {
	if (errors.length === 0) {
		return "All files are valid.";
	}

	const lines: string[] = [];
	lines.push("");
	lines.push(`  Found ${errors.length} validation error(s):`);
	lines.push("");

	for (const error of errors) {
		lines.push(`  ✗ ${error.file}: ${error.message}`);
	}

	lines.push("");
	return lines.join("\n");
}

function formatValue(value: unknown): string {
	if (value === undefined || value === null) return "(none)";
	if (typeof value === "string") {
		if (value.length > 60) {
			return `"${value.slice(0, 57)}..."`;
		}
		return `"${value}"`;
	}
	if (Array.isArray(value)) {
		return `[${value.join(", ")}]`;
	}
	return String(value);
}
