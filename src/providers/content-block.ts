import { readFile } from "node:fs/promises";
import { basename } from "node:path";
import { stringify } from "yaml";
import type { BrazeClient } from "../core/braze-client.js";
import { compareStringArrays, compareStrings, computeDiff } from "../core/diff-engine.js";
import type { ApplyOptions, ApplyResult, DiffResult, ValidationError } from "../types/diff.js";
import type { ContentBlockDefinition, LocalFileOutput } from "../types/resource.js";
import type { Provider } from "./base.js";
import { globFiles, parseFrontmatter } from "./utils.js";

interface RemoteContentBlock {
  name: string;
  content_block_id: string;
  content: string;
  description: string;
  state: "active" | "draft";
  tags: string[];
}

export class ContentBlockProvider implements Provider<ContentBlockDefinition, RemoteContentBlock> {
  readonly resourceType = "content_block";

  async readLocal(basePath: string): Promise<ContentBlockDefinition[]> {
    const files = await globFiles(basePath, ".liquid");
    const results: ContentBlockDefinition[] = [];

    for (const file of files) {
      const raw = await readFile(file, "utf-8");
      const { frontmatter, body } = parseFrontmatter(raw);
      const name = basename(file, ".liquid");

      results.push({
        name,
        content: body,
        description:
          typeof frontmatter.description === "string" ? frontmatter.description : undefined,
        state:
          frontmatter.state === "active" || frontmatter.state === "draft"
            ? frontmatter.state
            : undefined,
        tags: Array.isArray(frontmatter.tags)
          ? frontmatter.tags.filter((t): t is string => typeof t === "string")
          : undefined,
      });
    }

    return results;
  }

  async fetchRemote(client: BrazeClient): Promise<RemoteContentBlock[]> {
    const blocks: RemoteContentBlock[] = [];
    let offset = 0;
    const limit = 1000;
    const concurrency = 5;

    // Paginate through all content blocks
    let hasMore = true;
    while (hasMore) {
      const listResponse = await client.listContentBlocks(limit, offset);
      if (!listResponse.content_blocks || listResponse.content_blocks.length === 0) {
        break;
      }

      // Fetch full info in batches with concurrency control
      const items = listResponse.content_blocks;
      const errors: string[] = [];
      for (let i = 0; i < items.length; i += concurrency) {
        const batch = items.slice(i, i + concurrency);
        const results = await Promise.allSettled(
          batch.map((item) => client.getContentBlockInfo(item.content_block_id)),
        );
        for (const result of results) {
          if (result.status === "fulfilled") {
            const info = result.value;
            blocks.push({
              name: info.name,
              content_block_id: info.content_block_id,
              content: info.content,
              description: info.description,
              state: info.state,
              tags: info.tags,
            });
          } else {
            errors.push(
              result.reason instanceof Error ? result.reason.message : String(result.reason),
            );
          }
        }
      }
      if (errors.length > 0) {
        throw new Error(`Failed to fetch ${errors.length} content block(s): ${errors.join("; ")}`);
      }

      if (listResponse.content_blocks.length < limit) {
        hasMore = false;
      } else {
        offset += limit;
      }
    }

    return blocks;
  }

  diff(local: ContentBlockDefinition[], remote: RemoteContentBlock[]): DiffResult[] {
    return computeDiff(this.resourceType, local, remote, (l, r) => {
      const details = [];

      const contentDiff = compareStrings("content", l.content, r.content);
      if (contentDiff) details.push(contentDiff);

      const descDiff = compareStrings("description", l.description, r.description);
      if (descDiff) details.push(descDiff);

      const stateDiff = compareStrings("state", l.state, r.state);
      if (stateDiff) details.push(stateDiff);

      const tagsDiff = compareStringArrays("tags", l.tags, r.tags);
      if (tagsDiff) details.push(tagsDiff);

      return details;
    });
  }

  async apply(
    client: BrazeClient,
    diffs: DiffResult[],
    options: ApplyOptions,
    localDefs: ContentBlockDefinition[],
    remoteItems: RemoteContentBlock[],
  ): Promise<ApplyResult[]> {
    const localMap = new Map(localDefs.map((d) => [d.name, d]));
    const remoteMap = new Map(remoteItems.map((b) => [b.name, b]));
    const results: ApplyResult[] = [];

    for (const diff of diffs) {
      if (diff.operation === "remove") {
        results.push({
          resourceType: this.resourceType,
          resourceName: diff.resourceName,
          operation: "remove",
          success: false,
          message:
            "Content block exists in Braze but not in local files. Manual deletion required (no API support).",
        });
        continue;
      }

      if (!options.confirm) {
        results.push({
          resourceType: this.resourceType,
          resourceName: diff.resourceName,
          operation: diff.operation,
          success: true,
          message: `Would ${diff.operation === "add" ? "create" : "update"} content block (dry-run)`,
        });
        continue;
      }

      const localDef = localMap.get(diff.resourceName);
      if (!localDef) {
        results.push({
          resourceType: this.resourceType,
          resourceName: diff.resourceName,
          operation: diff.operation,
          success: false,
          message: "Local definition not found",
        });
        continue;
      }

      try {
        if (diff.operation === "add") {
          await client.createContentBlock({
            name: localDef.name,
            content: localDef.content,
            description: localDef.description,
            state: localDef.state,
            tags: localDef.tags,
          });
          results.push({
            resourceType: this.resourceType,
            resourceName: diff.resourceName,
            operation: "add",
            success: true,
            message: "Created content block",
          });
        } else if (diff.operation === "change") {
          const remote = remoteMap.get(diff.resourceName);
          if (!remote) {
            results.push({
              resourceType: this.resourceType,
              resourceName: diff.resourceName,
              operation: "change",
              success: false,
              message: "Remote content block not found for update",
            });
            continue;
          }
          await client.updateContentBlock({
            content_block_id: remote.content_block_id,
            name: localDef.name,
            content: localDef.content,
            description: localDef.description,
            state: localDef.state,
            tags: localDef.tags,
          });
          results.push({
            resourceType: this.resourceType,
            resourceName: diff.resourceName,
            operation: "change",
            success: true,
            message: "Updated content block",
          });
        }
      } catch (e) {
        results.push({
          resourceType: this.resourceType,
          resourceName: diff.resourceName,
          operation: diff.operation,
          success: false,
          message: e instanceof Error ? e.message : String(e),
        });
      }
    }

    return results;
  }

  serialize(remote: RemoteContentBlock): LocalFileOutput {
    const meta: Record<string, unknown> = {};
    if (remote.description) {
      meta.description = remote.description;
    }
    if (remote.state) {
      meta.state = remote.state;
    }
    if (remote.tags && remote.tags.length > 0) {
      meta.tags = [...remote.tags].sort();
    }

    let content = "";
    if (Object.keys(meta).length > 0) {
      const yaml = stringify(meta, { sortMapEntries: true }).trimEnd();
      content = `---\n${yaml}\n---\n${remote.content}`;
    } else {
      content = remote.content;
    }

    return {
      path: `${remote.name}.liquid`,
      content,
    };
  }

  validate(local: ContentBlockDefinition[]): ValidationError[] {
    const errors: ValidationError[] = [];
    const names = new Set<string>();

    for (const block of local) {
      const file = `${block.name}.liquid`;

      if (!block.name) {
        errors.push({ file, message: "Content block must have a name" });
      }

      if (!block.content || block.content.trim() === "") {
        errors.push({ file, message: "Content block must have content" });
      }

      if (names.has(block.name)) {
        errors.push({ file, message: `Duplicate content block name: ${block.name}` });
      }
      names.add(block.name);

      if (block.state && block.state !== "active" && block.state !== "draft") {
        errors.push({
          file,
          message: `Invalid state '${block.state}'. Must be 'active' or 'draft'`,
        });
      }

      if (block.tags) {
        for (const tag of block.tags) {
          if (typeof tag !== "string") {
            errors.push({
              file,
              message: `Invalid tag value: ${JSON.stringify(tag)}. Tags must be strings`,
            });
          }
        }
      }
    }

    return errors;
  }
}
