import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { ContentBlockProvider } from "../../../src/providers/content-block.js";
import type { ContentBlockDefinition } from "../../../src/types/resource.js";

const fixturesDir = join(import.meta.dirname, "../../fixtures/content_blocks");

describe("ContentBlockProvider", () => {
	const provider = new ContentBlockProvider();

	describe("readLocal", () => {
		it("reads liquid files with frontmatter", async () => {
			const blocks = await provider.readLocal(fixturesDir);
			expect(blocks).toHaveLength(1);
			expect(blocks[0].name).toBe("bonus_dialog");
			expect(blocks[0].description).toBe("Post bonus dialog content");
			expect(blocks[0].state).toBe("active");
			expect(blocks[0].tags).toEqual(["campaign_2504", "pr"]);
			expect(blocks[0].content).toContain("bonus-dialog");
		});

		it("returns empty array for missing directory", async () => {
			const blocks = await provider.readLocal("/nonexistent");
			expect(blocks).toHaveLength(0);
		});
	});

	describe("diff", () => {
		const localBlocks: ContentBlockDefinition[] = [
			{
				name: "bonus_dialog",
				content: "<div>new content</div>\n",
				description: "Updated description",
				state: "active",
				tags: ["tag1"],
			},
		];

		it("detects new content block", () => {
			const diffs = provider.diff(localBlocks, []);
			expect(diffs).toHaveLength(1);
			expect(diffs[0].operation).toBe("add");
		});

		it("detects content changes", () => {
			const remote = [
				{
					name: "bonus_dialog",
					content_block_id: "cb-123",
					content: "<div>old content</div>\n",
					description: "Updated description",
					state: "active" as const,
					tags: ["tag1"],
				},
			];
			const diffs = provider.diff(localBlocks, remote);
			expect(diffs).toHaveLength(1);
			expect(diffs[0].operation).toBe("change");
			expect(diffs[0].details.some((d) => d.field === "content")).toBe(true);
		});

		it("detects no diff when in sync", () => {
			const remote = [
				{
					name: "bonus_dialog",
					content_block_id: "cb-123",
					content: "<div>new content</div>\n",
					description: "Updated description",
					state: "active" as const,
					tags: ["tag1"],
				},
			];
			const diffs = provider.diff(localBlocks, remote);
			expect(diffs).toHaveLength(0);
		});
	});

	describe("validate", () => {
		it("passes valid content block", () => {
			const errors = provider.validate([{ name: "test", content: "<div></div>" }]);
			expect(errors).toHaveLength(0);
		});

		it("rejects duplicate names", () => {
			const errors = provider.validate([
				{ name: "test", content: "a" },
				{ name: "test", content: "b" },
			]);
			expect(errors.some((e) => e.message.includes("Duplicate"))).toBe(true);
		});

		it("rejects invalid state", () => {
			const errors = provider.validate([
				{ name: "test", content: "a", state: "invalid" as "active" },
			]);
			expect(errors.some((e) => e.message.includes("Invalid state"))).toBe(true);
		});
	});

	describe("serialize", () => {
		it("produces liquid file with frontmatter", () => {
			const output = provider.serialize({
				name: "test_block",
				content_block_id: "cb-123",
				content: "<div>Hello</div>",
				description: "Test block",
				state: "active",
				tags: ["b_tag", "a_tag"],
			});
			expect(output.path).toBe("test_block.liquid");
			expect(output.content).toContain("---");
			expect(output.content).toContain("description:");
			expect(output.content).toContain("<div>Hello</div>");
			// Tags should be sorted
			const aIndex = output.content.indexOf("a_tag");
			const bIndex = output.content.indexOf("b_tag");
			expect(aIndex).toBeLessThan(bIndex);
		});

		it("produces content without frontmatter when no metadata", () => {
			const output = provider.serialize({
				name: "simple",
				content_block_id: "cb-456",
				content: "Hello",
				description: "",
				state: "active",
				tags: [],
			});
			// state "active" should still appear in frontmatter
			expect(output.content).toContain("state: active");
		});
	});
});
