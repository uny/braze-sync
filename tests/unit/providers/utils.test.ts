import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { parseFrontmatter, safePath } from "../../../src/providers/utils.js";

describe("parseFrontmatter", () => {
  it("parses standard frontmatter with LF line endings", () => {
    const raw = "---\ndescription: test\nstate: active\n---\n<div>body</div>";
    const { frontmatter, body } = parseFrontmatter(raw);
    expect(frontmatter.description).toBe("test");
    expect(frontmatter.state).toBe("active");
    expect(body).toBe("<div>body</div>");
  });

  it("parses frontmatter with CRLF line endings", () => {
    const raw = "---\r\ndescription: test\r\nstate: active\r\n---\r\n<div>body</div>";
    const { frontmatter, body } = parseFrontmatter(raw);
    expect(frontmatter.description).toBe("test");
    expect(frontmatter.state).toBe("active");
    expect(body).toBe("<div>body</div>");
  });

  it("returns raw content when no frontmatter", () => {
    const raw = "<div>no frontmatter</div>";
    const { frontmatter, body } = parseFrontmatter(raw);
    expect(frontmatter).toEqual({});
    expect(body).toBe(raw);
  });

  it("returns empty frontmatter for non-mapping YAML (array)", () => {
    const raw = "---\n- item1\n- item2\n---\nbody";
    const { frontmatter, body } = parseFrontmatter(raw);
    expect(frontmatter).toEqual({});
    expect(body).toBe("body");
  });

  it("returns empty frontmatter for scalar YAML", () => {
    const raw = "---\njust a string\n---\nbody";
    const { frontmatter, body } = parseFrontmatter(raw);
    expect(frontmatter).toEqual({});
    expect(body).toBe("body");
  });
});

describe("safePath", () => {
  const base = "/tmp/test-base";

  it("resolves a simple filename within base", () => {
    const result = safePath(base, "file.yaml");
    expect(result).toBe(resolve(base, "file.yaml"));
  });

  it("resolves a subdirectory path within base", () => {
    const result = safePath(base, "sub/file.yaml");
    expect(result).toBe(resolve(base, "sub/file.yaml"));
  });

  it("allows the base directory itself", () => {
    const result = safePath(base, ".");
    expect(result).toBe(resolve(base));
  });

  it("throws on ../ traversal", () => {
    expect(() => safePath(base, "../escape.yaml")).toThrow("Path traversal detected");
  });

  it("throws on nested ../ traversal", () => {
    expect(() => safePath(base, "sub/../../escape.yaml")).toThrow("Path traversal detected");
  });

  it("throws on absolute path outside base", () => {
    expect(() => safePath(base, "/etc/passwd")).toThrow("Path traversal detected");
  });

  it("allows absolute path that resolves within base", () => {
    const result = safePath(base, `${base}/nested/file.yaml`);
    expect(result).toBe(resolve(base, "nested/file.yaml"));
  });
});
