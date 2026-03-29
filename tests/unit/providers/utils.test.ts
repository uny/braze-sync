import { describe, expect, it } from "vitest";
import { parseFrontmatter } from "../../../src/providers/utils.js";

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
});
