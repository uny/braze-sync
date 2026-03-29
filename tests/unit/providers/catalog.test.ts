import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { CatalogProvider } from "../../../src/providers/catalog.js";
import type { CatalogDefinition } from "../../../src/types/resource.js";

const fixturesDir = join(import.meta.dirname, "../../fixtures/catalogs");

describe("CatalogProvider", () => {
	const provider = new CatalogProvider();

	describe("readLocal", () => {
		it("reads catalog YAML files from directory", async () => {
			const catalogs = await provider.readLocal(fixturesDir);
			expect(catalogs).toHaveLength(1);
			expect(catalogs[0].name).toBe("cardiology");
			expect(catalogs[0].fields).toHaveLength(4);
		});

		it("returns empty array for missing directory", async () => {
			const catalogs = await provider.readLocal("/nonexistent");
			expect(catalogs).toHaveLength(0);
		});
	});

	describe("diff", () => {
		const localCatalogs: CatalogDefinition[] = [
			{
				name: "cardiology",
				description: "Cardiology catalog",
				fields: [
					{ name: "condition_id", type: "string" },
					{ name: "condition_name", type: "string" },
					{ name: "new_field", type: "number" },
				],
			},
		];

		it("detects new catalog", () => {
			const diffs = provider.diff(localCatalogs, []);
			expect(diffs).toHaveLength(1);
			expect(diffs[0].operation).toBe("add");
		});

		it("detects field additions", () => {
			const remote = [
				{
					name: "cardiology",
					description: "Cardiology catalog",
					fields: [
						{ name: "condition_id", type: "string" },
						{ name: "condition_name", type: "string" },
					],
				},
			];
			const diffs = provider.diff(localCatalogs, remote);
			expect(diffs).toHaveLength(1);
			expect(diffs[0].operation).toBe("change");
			expect(diffs[0].details.some((d) => d.field === "fields.new_field")).toBe(true);
		});

		it("detects removed catalogs in remote", () => {
			const remote = [
				{
					name: "cardiology",
					description: "Cardiology catalog",
					fields: [
						{ name: "condition_id", type: "string" },
						{ name: "condition_name", type: "string" },
						{ name: "new_field", type: "number" },
					],
				},
				{
					name: "oncology",
					description: "Oncology catalog",
					fields: [{ name: "id", type: "string" }],
				},
			];
			const diffs = provider.diff(localCatalogs, remote);
			expect(diffs.some((d) => d.operation === "remove" && d.resourceName === "oncology")).toBe(
				true,
			);
		});
	});

	describe("validate", () => {
		it("passes valid catalog", () => {
			const errors = provider.validate([
				{
					name: "test",
					description: "Test",
					fields: [{ name: "f1", type: "string" }],
				},
			]);
			expect(errors).toHaveLength(0);
		});

		it("rejects invalid field type", () => {
			const errors = provider.validate([
				{
					name: "test",
					description: "Test",
					fields: [{ name: "f1", type: "invalid" as "string" }],
				},
			]);
			expect(errors.length).toBeGreaterThan(0);
			expect(errors[0].message).toContain("Invalid field type");
		});

		it("rejects empty fields", () => {
			const errors = provider.validate([
				{
					name: "test",
					description: "Test",
					fields: [],
				},
			]);
			expect(errors.length).toBeGreaterThan(0);
		});

		it("rejects duplicate catalog names", () => {
			const errors = provider.validate([
				{
					name: "test",
					description: "Test",
					fields: [{ name: "f1", type: "string" }],
				},
				{
					name: "test",
					description: "Test2",
					fields: [{ name: "f2", type: "number" }],
				},
			]);
			expect(errors.some((e) => e.message.includes("Duplicate"))).toBe(true);
		});

		it("rejects duplicate field names", () => {
			const errors = provider.validate([
				{
					name: "test",
					description: "Test",
					fields: [
						{ name: "f1", type: "string" },
						{ name: "f1", type: "number" },
					],
				},
			]);
			expect(errors.some((e) => e.message.includes("Duplicate field"))).toBe(true);
		});
	});

	describe("serialize", () => {
		it("produces deterministic YAML output", () => {
			const output = provider.serialize({
				name: "test",
				description: "Test catalog",
				fields: [
					{ name: "z_field", type: "string" },
					{ name: "a_field", type: "number" },
				],
			});
			expect(output.path).toBe("test.yaml");
			expect(output.content).toContain("a_field");
			// Fields should be sorted
			const aIndex = output.content.indexOf("a_field");
			const zIndex = output.content.indexOf("z_field");
			expect(aIndex).toBeLessThan(zIndex);
		});
	});
});
