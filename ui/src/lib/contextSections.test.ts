import { describe, it, expect } from "vitest";
import { parseDataUrl } from "./contextSections";

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
