import type { DiffDetail, DiffOperation, DiffResult } from "../types/diff.js";

export interface Named {
  name: string;
}

/**
 * Generic diff between local and remote resource lists.
 * Matches resources by name.
 */
export function computeDiff<L extends Named, R extends Named>(
  resourceType: string,
  local: L[],
  remote: R[],
  compareFields: (local: L, remote: R) => DiffDetail[],
): DiffResult[] {
  const results: DiffResult[] = [];
  const remoteMap = new Map(remote.map((r) => [r.name, r]));
  const localMap = new Map(local.map((l) => [l.name, l]));

  // Resources in local but not in remote → add
  for (const l of local) {
    const r = remoteMap.get(l.name);
    if (!r) {
      results.push({
        resourceType,
        resourceName: l.name,
        operation: "add",
        details: [],
      });
    } else {
      // Exists in both → check for changes
      const details = compareFields(l, r);
      if (details.length > 0) {
        results.push({
          resourceType,
          resourceName: l.name,
          operation: "change",
          details,
        });
      }
    }
  }

  // Resources in remote but not in local → remove
  for (const r of remote) {
    if (!localMap.has(r.name)) {
      results.push({
        resourceType,
        resourceName: r.name,
        operation: "remove",
        details: [],
      });
    }
  }

  return results;
}

/**
 * Compare two arrays of fields (by name).
 * Returns diff details for added, removed, and changed fields.
 */
export function compareFieldArrays(
  localFields: Array<{ name: string; type: string }>,
  remoteFields: Array<{ name: string; type: string }>,
  parentPath = "fields",
): DiffDetail[] {
  const details: DiffDetail[] = [];
  const remoteMap = new Map(remoteFields.map((f) => [f.name, f]));
  const localMap = new Map(localFields.map((f) => [f.name, f]));

  for (const lf of localFields) {
    const rf = remoteMap.get(lf.name);
    if (!rf) {
      details.push({
        field: `${parentPath}.${lf.name}`,
        operation: "add",
        localValue: lf.type,
      });
    } else if (lf.type !== rf.type) {
      details.push({
        field: `${parentPath}.${lf.name}.type`,
        operation: "change",
        localValue: lf.type,
        remoteValue: rf.type,
      });
    }
  }

  for (const rf of remoteFields) {
    if (!localMap.has(rf.name)) {
      details.push({
        field: `${parentPath}.${rf.name}`,
        operation: "remove",
        remoteValue: rf.type,
      });
    }
  }

  return details;
}

/**
 * Simple string diff — returns a single change detail if strings differ.
 */
export function compareStrings(
  field: string,
  local: string | undefined,
  remote: string | undefined,
): DiffDetail | null {
  const l = local ?? "";
  const r = remote ?? "";
  if (l !== r) {
    return { field, operation: "change", localValue: l, remoteValue: r };
  }
  return null;
}

/**
 * Compare two arrays of strings (e.g., tags). Order-insensitive.
 */
export function compareStringArrays(
  field: string,
  local: string[] | undefined,
  remote: string[] | undefined,
): DiffDetail | null {
  const l = [...(local ?? [])].sort();
  const r = [...(remote ?? [])].sort();
  if (JSON.stringify(l) !== JSON.stringify(r)) {
    return { field, operation: "change", localValue: l, remoteValue: r };
  }
  return null;
}

export function operationSymbol(op: DiffOperation): string {
  switch (op) {
    case "add":
      return "+";
    case "remove":
      return "-";
    case "change":
      return "~";
  }
}
