import { describe, it, expect } from "vitest";
import { lineRange, parseDataUrl } from "./contextSections";

describe("parseDataUrl", () => {
  it("keeps the full data URL, which is what the backend parses", () => {
    expect(parseDataUrl("data:image/png;base64,AAAA")).toEqual({
      dataUrl: "data:image/png;base64,AAAA",
    });
  });

  it("tolerates surrounding whitespace", () => {
    expect(parseDataUrl("  data:image/jpeg;base64,QQ==  ")).toEqual({
      dataUrl: "data:image/jpeg;base64,QQ==",
    });
  });

  it("accepts other media types", () => {
    expect(parseDataUrl("data:image/webp;base64,QQ==")?.dataUrl).toBe(
      "data:image/webp;base64,QQ==",
    );
  });

  it("rejects a bare base64 payload with no data URL prefix", () => {
    expect(parseDataUrl("AAAA")).toBeNull();
  });

  it("rejects a non-base64 data URL", () => {
    expect(parseDataUrl("data:image/png,AAAA")).toBeNull();
  });

  it("rejects an empty string", () => {
    expect(parseDataUrl("")).toBeNull();
    expect(parseDataUrl("   ")).toBeNull();
  });
});

describe("lineRange", () => {
  const content = "a1\nb2\nc3\nd4\ne5";

  it("returns the requested inclusive, 1-based range with line numbers", () => {
    expect(lineRange(content, 2, 4)).toBe("2 | b2\n3 | c3\n4 | d4");
  });

  it("defaults to the whole file when no boundaries are given", () => {
    expect(lineRange(content)).toBe("1 | a1\n2 | b2\n3 | c3\n4 | d4\n5 | e5");
  });

  it("keeps a single line when start equals end", () => {
    expect(lineRange(content, 3, 3)).toBe("3 | c3");
  });

  it("clamps a too-far end to the last line", () => {
    expect(lineRange(content, 4, 99)).toBe("4 | d4\n5 | e5");
  });

  it("clamps a too-small start to line 1", () => {
    expect(lineRange(content, -2, 2)).toBe("1 | a1\n2 | b2");
  });

  it("returns empty when the range starts past the last line", () => {
    expect(lineRange(content, 6, 9)).toBe("");
    expect(lineRange("", 1, 3)).toBe("");
  });

  it("aligns line-number columns so multi-digit ranges stay readable", () => {
    const big = Array.from({ length: 15 }, (_, i) => `line${i + 1}`).join("\n");
    expect(lineRange(big, 10, 12)).toBe("10 | line10\n11 | line11\n12 | line12");
  });
});
