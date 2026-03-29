import { readFile } from "node:fs/promises";
import { basename } from "node:path";
import { parse, stringify } from "yaml";
import type { BrazeClient } from "../core/braze-client.js";
import { compareFieldArrays, compareStrings, computeDiff } from "../core/diff-engine.js";
import type { ApplyOptions, ApplyResult, DiffResult, ValidationError } from "../types/diff.js";
import type { CatalogDefinition, LocalFileOutput } from "../types/resource.js";
import type { Provider } from "./base.js";
import { globYaml } from "./utils.js";

interface RemoteCatalog {
	name: string;
	description: string;
	fields: Array<{ name: string; type: string }>;
}

const VALID_FIELD_TYPES = new Set(["string", "number", "boolean", "time"]);

export class CatalogProvider implements Provider<CatalogDefinition, RemoteCatalog> {
	readonly resourceType = "catalog";

	async readLocal(basePath: string): Promise<CatalogDefinition[]> {
		const files = await globYaml(basePath);
		const results: CatalogDefinition[] = [];

		for (const file of files) {
			const raw = await readFile(file, "utf-8");
			const data = parse(raw) as CatalogDefinition;
			// Use filename as name if not specified
			if (!data.name) {
				data.name = basename(file, ".yaml").replace(/\.yml$/, "");
			}
			results.push(data);
		}

		return results;
	}

	async fetchRemote(client: BrazeClient): Promise<RemoteCatalog[]> {
		const response = await client.listCatalogs();
		return response.catalogs.map((c) => ({
			name: c.name,
			description: c.description,
			// Filter out the auto-created 'id' field
			fields: c.fields.filter((f) => f.name !== "id").map((f) => ({ name: f.name, type: f.type })),
		}));
	}

	diff(local: CatalogDefinition[], remote: RemoteCatalog[]): DiffResult[] {
		return computeDiff(this.resourceType, local, remote, (l, r) => {
			const details = [];

			const descDiff = compareStrings("description", l.description, r.description);
			if (descDiff) details.push(descDiff);

			const fieldDiffs = compareFieldArrays(l.fields, r.fields);
			details.push(...fieldDiffs);

			return details;
		});
	}

	async apply(
		client: BrazeClient,
		diffs: DiffResult[],
		options: ApplyOptions,
	): Promise<ApplyResult[]> {
		const results: ApplyResult[] = [];

		for (const diff of diffs) {
			if (diff.operation === "add") {
				// "add" requires full local definition — use applyWithLocal() instead
				results.push({
					resourceType: this.resourceType,
					resourceName: diff.resourceName,
					operation: "add",
					success: true,
					message: "Would create catalog (dry-run)",
				});
			} else if (diff.operation === "change") {
				for (const detail of diff.details) {
					if (detail.operation === "add") {
						if (!options.confirm) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: true,
								message: `Would add field ${detail.field} (dry-run)`,
							});
							continue;
						}
						const fieldName = detail.field.replace("fields.", "");
						try {
							await client.createCatalogFields(diff.resourceName, {
								fields: [{ name: fieldName, type: detail.localValue as "string" }],
							});
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: true,
								message: `Added field ${fieldName}`,
							});
						} catch (e) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: false,
								message: e instanceof Error ? e.message : String(e),
							});
						}
					} else if (detail.operation === "remove") {
						if (!options.allowDestructive) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: false,
								message: `Skipped removing field ${detail.field} (requires --allow-destructive)`,
							});
							continue;
						}
						if (!options.confirm) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: true,
								message: `Would remove field ${detail.field} (dry-run)`,
							});
							continue;
						}
						const fieldName = detail.field.replace("fields.", "");
						try {
							await client.deleteCatalogField(diff.resourceName, fieldName);
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: true,
								message: `Deleted field ${fieldName}`,
							});
						} catch (e) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: false,
								message: e instanceof Error ? e.message : String(e),
							});
						}
					} else if (detail.operation === "change") {
						// Field type change requires delete + recreate
						if (!options.allowDestructive) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: false,
								message: `Skipped changing field type for ${detail.field} (requires --allow-destructive)`,
							});
							continue;
						}
						if (!options.confirm) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: true,
								message: `Would change field type ${detail.field}: ${detail.remoteValue} → ${detail.localValue} (dry-run)`,
							});
							continue;
						}
						const fieldName = detail.field.replace("fields.", "").replace(".type", "");
						try {
							await client.deleteCatalogField(diff.resourceName, fieldName);
							await client.createCatalogFields(diff.resourceName, {
								fields: [{ name: fieldName, type: detail.localValue as "string" }],
							});
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: true,
								message: `Changed field type ${fieldName}: ${detail.remoteValue} → ${detail.localValue}`,
							});
						} catch (e) {
							results.push({
								resourceType: this.resourceType,
								resourceName: diff.resourceName,
								operation: "change",
								success: false,
								message: e instanceof Error ? e.message : String(e),
							});
						}
					}
				}
			} else if (diff.operation === "remove") {
				// Braze has no catalog delete API — just warn
				results.push({
					resourceType: this.resourceType,
					resourceName: diff.resourceName,
					operation: "remove",
					success: false,
					message:
						"Catalog exists in Braze but not in local files. Manual deletion required (no API support).",
				});
			}
		}

		return results;
	}

	serialize(remote: RemoteCatalog): LocalFileOutput {
		const data: CatalogDefinition = {
			name: remote.name,
			description: remote.description,
			fields: remote.fields
				.map((f) => ({ name: f.name, type: f.type as CatalogDefinition["fields"][0]["type"] }))
				.sort((a, b) => a.name.localeCompare(b.name)),
		};

		return {
			path: `${remote.name}.yaml`,
			content: stringify(data, { sortMapEntries: true }),
		};
	}

	validate(local: CatalogDefinition[]): ValidationError[] {
		const errors: ValidationError[] = [];
		const names = new Set<string>();

		for (const catalog of local) {
			const file = `${catalog.name}.yaml`;

			if (!catalog.name) {
				errors.push({ file, message: "Catalog must have a 'name' field" });
			}

			if (names.has(catalog.name)) {
				errors.push({ file, message: `Duplicate catalog name: ${catalog.name}` });
			}
			names.add(catalog.name);

			if (!Array.isArray(catalog.fields) || catalog.fields.length === 0) {
				errors.push({ file, message: "Catalog must have at least one field" });
			}

			const fieldNames = new Set<string>();
			for (const field of catalog.fields ?? []) {
				if (!field.name) {
					errors.push({ file, message: "Field must have a 'name'" });
				}
				if (!VALID_FIELD_TYPES.has(field.type)) {
					errors.push({
						file,
						message: `Invalid field type '${field.type}' for field '${field.name}'. Valid types: ${[...VALID_FIELD_TYPES].join(", ")}`,
					});
				}
				if (fieldNames.has(field.name)) {
					errors.push({ file, message: `Duplicate field name: ${field.name}` });
				}
				fieldNames.add(field.name);
			}
		}

		return errors;
	}

	async applyWithLocal(
		client: BrazeClient,
		diffs: DiffResult[],
		options: ApplyOptions,
		localDefs: CatalogDefinition[],
	): Promise<ApplyResult[]> {
		const localMap = new Map(localDefs.map((d) => [d.name, d]));
		const results: ApplyResult[] = [];

		for (const diff of diffs) {
			if (diff.operation === "add") {
				const localDef = localMap.get(diff.resourceName);
				if (!options.confirm) {
					results.push({
						resourceType: this.resourceType,
						resourceName: diff.resourceName,
						operation: "add",
						success: true,
						message: "Would create catalog (dry-run)",
					});
					continue;
				}
				if (!localDef) {
					results.push({
						resourceType: this.resourceType,
						resourceName: diff.resourceName,
						operation: "add",
						success: false,
						message: "Local definition not found",
					});
					continue;
				}
				try {
					await client.createCatalog({
						catalogs: [
							{
								name: localDef.name,
								description: localDef.description,
								fields: localDef.fields.map((f) => ({
									name: f.name,
									type: f.type,
								})),
							},
						],
					});
					results.push({
						resourceType: this.resourceType,
						resourceName: diff.resourceName,
						operation: "add",
						success: true,
						message: "Created catalog",
					});
				} catch (e) {
					results.push({
						resourceType: this.resourceType,
						resourceName: diff.resourceName,
						operation: "add",
						success: false,
						message: e instanceof Error ? e.message : String(e),
					});
				}
			} else {
				// Delegate change/remove to regular apply
				const subResults = await this.apply(client, [diff], options);
				results.push(...subResults);
			}
		}

		return results;
	}
}
