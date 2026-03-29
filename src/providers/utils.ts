import { readdir } from "node:fs/promises";
import { isAbsolute, join, relative, resolve } from "node:path";
import { parse } from "yaml";

export async function globYaml(dirPath: string): Promise<string[]> {
  try {
    const entries = await readdir(dirPath, { withFileTypes: true });
    return entries
      .filter((e) => e.isFile() && (e.name.endsWith(".yaml") || e.name.endsWith(".yml")))
      .map((e) => join(dirPath, e.name))
      .sort();
  } catch (e) {
    if (isNodeError(e) && e.code === "ENOENT") {
      console.error(`Warning: Resource directory not found: ${dirPath}`);
      return [];
    }
    throw e;
  }
}

export async function globFiles(dirPath: string, extension: string): Promise<string[]> {
  try {
    const entries = await readdir(dirPath, { withFileTypes: true });
    return entries
      .filter((e) => e.isFile() && e.name.endsWith(extension))
      .map((e) => join(dirPath, e.name))
      .sort();
  } catch (e) {
    if (isNodeError(e) && e.code === "ENOENT") {
      console.error(`Warning: Resource directory not found: ${dirPath}`);
      return [];
    }
    throw e;
  }
}

function isNodeError(e: unknown): e is NodeJS.ErrnoException {
  return e instanceof Error && "code" in e;
}

/**
 * Resolve a relative path within a base directory, preventing path traversal.
 * Throws if the resolved path escapes the base directory.
 */
export function safePath(basePath: string, relativePath: string): string {
  const resolved = resolve(basePath, relativePath);
  const resolvedBase = resolve(basePath);
  const rel = relative(resolvedBase, resolved);
  if (rel.startsWith("..") || isAbsolute(rel)) {
    throw new Error(`Path traversal detected: '${relativePath}' escapes base directory`);
  }
  return resolved;
}

/**
 * Parse frontmatter from a file content.
 * Returns the frontmatter as a parsed object and the remaining body content.
 */
export function parseFrontmatter(raw: string): {
  frontmatter: Record<string, unknown>;
  body: string;
} {
  const match = raw.match(/^---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)$/);
  if (!match) {
    return { frontmatter: {}, body: raw };
  }

  const parsed = parse(match[1]);
  const frontmatter =
    parsed !== null && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : {};
  const body = match[2];

  return { frontmatter, body };
}
