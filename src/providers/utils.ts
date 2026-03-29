import { readdir } from "node:fs/promises";
import { join } from "node:path";

export async function globYaml(dirPath: string): Promise<string[]> {
	try {
		const entries = await readdir(dirPath, { withFileTypes: true });
		return entries
			.filter((e) => e.isFile() && (e.name.endsWith(".yaml") || e.name.endsWith(".yml")))
			.map((e) => join(dirPath, e.name))
			.sort();
	} catch {
		return [];
	}
}

export async function globFiles(dirPath: string, extension: string): Promise<string[]> {
	try {
		const entries = await readdir(dirPath, { withFileTypes: true });
		return entries
			.filter((e) => e.isFile() && e.name.endsWith(extension))
			.map((e) => join(dirPath, e.name))
			.sort();
	} catch {
		return [];
	}
}

/**
 * Parse frontmatter from a file content.
 * Returns the frontmatter as a parsed object and the remaining body content.
 */
export function parseFrontmatter(content: string): {
	frontmatter: Record<string, unknown>;
	body: string;
} {
	const match = content.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
	if (!match) {
		return { frontmatter: {}, body: content };
	}

	const { parse } = require("yaml") as typeof import("yaml");
	const frontmatter = parse(match[1]) as Record<string, unknown>;
	const body = match[2];

	return { frontmatter, body };
}
