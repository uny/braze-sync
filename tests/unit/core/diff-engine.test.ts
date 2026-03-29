import { describe, expect, it } from "vitest";
import {
  compareFieldArrays,
  compareStringArrays,
  compareStrings,
  computeDiff,
} from "../../../src/core/diff-engine.js";

describe("computeDiff", () => {
  it("detects additions", () => {
    const local = [{ name: "a" }, { name: "b" }];
    const remote = [{ name: "a" }];
    const diffs = computeDiff("test", local, remote, () => []);

    expect(diffs).toHaveLength(1);
    expect(diffs[0].operation).toBe("add");
    expect(diffs[0].resourceName).toBe("b");
  });

  it("detects removals", () => {
    const local = [{ name: "a" }];
    const remote = [{ name: "a" }, { name: "b" }];
    const diffs = computeDiff("test", local, remote, () => []);

    expect(diffs).toHaveLength(1);
    expect(diffs[0].operation).toBe("remove");
    expect(diffs[0].resourceName).toBe("b");
  });

  it("detects changes", () => {
    const local = [{ name: "a", value: "new" }];
    const remote = [{ name: "a", value: "old" }];
    const diffs = computeDiff("test", local, remote, () => [
      { field: "value", operation: "change", localValue: "new", remoteValue: "old" },
    ]);

    expect(diffs).toHaveLength(1);
    expect(diffs[0].operation).toBe("change");
    expect(diffs[0].details).toHaveLength(1);
  });

  it("reports no diff when in sync", () => {
    const local = [{ name: "a" }];
    const remote = [{ name: "a" }];
    const diffs = computeDiff("test", local, remote, () => []);

    expect(diffs).toHaveLength(0);
  });
});

describe("compareFieldArrays", () => {
  it("detects added fields", () => {
    const local = [
      { name: "a", type: "string" },
      { name: "b", type: "number" },
    ];
    const remote = [{ name: "a", type: "string" }];
    const details = compareFieldArrays(local, remote);

    expect(details).toHaveLength(1);
    expect(details[0].operation).toBe("add");
    expect(details[0].field).toBe("fields.b");
  });

  it("detects removed fields", () => {
    const local = [{ name: "a", type: "string" }];
    const remote = [
      { name: "a", type: "string" },
      { name: "b", type: "number" },
    ];
    const details = compareFieldArrays(local, remote);

    expect(details).toHaveLength(1);
    expect(details[0].operation).toBe("remove");
    expect(details[0].field).toBe("fields.b");
  });

  it("detects type changes", () => {
    const local = [{ name: "a", type: "number" }];
    const remote = [{ name: "a", type: "string" }];
    const details = compareFieldArrays(local, remote);

    expect(details).toHaveLength(1);
    expect(details[0].operation).toBe("change");
    expect(details[0].localValue).toBe("number");
    expect(details[0].remoteValue).toBe("string");
  });
});

describe("compareStrings", () => {
  it("returns null for equal strings", () => {
    expect(compareStrings("f", "hello", "hello")).toBeNull();
  });

  it("returns diff for different strings", () => {
    const result = compareStrings("f", "new", "old");
    expect(result).not.toBeNull();
    expect(result?.localValue).toBe("new");
    expect(result?.remoteValue).toBe("old");
  });

  it("treats undefined as empty string", () => {
    expect(compareStrings("f", undefined, "")).toBeNull();
  });
});

describe("compareStringArrays", () => {
  it("returns null for equal arrays regardless of order", () => {
    expect(compareStringArrays("f", ["b", "a"], ["a", "b"])).toBeNull();
  });

  it("detects differences", () => {
    const result = compareStringArrays("f", ["a", "c"], ["a", "b"]);
    expect(result).not.toBeNull();
  });
});
