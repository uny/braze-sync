import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { parse } from "yaml";

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
export function parseFrontmatter(raw: string): {
	frontmatter: Record<string, unknown>;
	body: string;
} {
	const match = raw.match(/^---\n([\s\S]*?)\n---\n?([\s\S]*)$/);
	if (!match) {
		return { frontmatter: {}, body: raw };
	}

	const frontmatter = parse(match[1]) as Record<string, unknown>;
	const body = match[2];

	return { frontmatter, body };
}
