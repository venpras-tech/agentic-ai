import { describe, expect, it } from "vitest";
import { activeMention, filterMentionFiles } from "./mention";

describe("filterMentionFiles", () => {
  const files = ["src/lib/a.ts", "src/components/Button.tsx", "README.md"];

  it("ranks basename prefix matches ahead of path substring matches", () => {
    expect(filterMentionFiles(files, "but")).toEqual(["src/components/Button.tsx"]);
  });

  it("returns a stable slice when the query is empty", () => {
    const out = filterMentionFiles(files, "", 2);
    expect(out).toHaveLength(2);
    expect(out.every((p) => files.includes(p))).toBe(true);
  });
});

describe("activeMention", () => {
  it("detects an @ token at the caret", () => {
    expect(activeMention("hello @src/foo", 14)).toEqual({ query: "src/foo", start: 6 });
  });

  it("returns null when @ is mid-word", () => {
    expect(activeMention("email@corp.com", 5)).toBeNull();
  });

  it("returns null when the token contains invalid characters", () => {
    expect(activeMention("see @foo, bar", 9)).toBeNull();
  });
});
