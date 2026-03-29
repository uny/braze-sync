import type { BrazeClient } from "../core/braze-client.js";
import type { ApplyOptions, ApplyResult, DiffResult, ValidationError } from "../types/diff.js";
import type { LocalFileOutput } from "../types/resource.js";

export interface Provider<TLocal, TRemote> {
	readonly resourceType: string;

	readLocal(basePath: string): Promise<TLocal[]>;

	fetchRemote(client: BrazeClient): Promise<TRemote[]>;

	diff(local: TLocal[], remote: TRemote[]): DiffResult[];

	apply(client: BrazeClient, diffs: DiffResult[], options: ApplyOptions): Promise<ApplyResult[]>;

	serialize(remote: TRemote): LocalFileOutput;

	validate(local: TLocal[]): ValidationError[];
}
