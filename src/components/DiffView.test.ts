import { describe, expect, it } from "vitest";

// Re-implement parseUnifiedDiff for testing (mirrors DiffView.tsx internal).
interface DiffLine {
  tag: "header" | "add" | "del" | "ctx" | "meta";
  text: string;
}

function parseUnifiedDiff(raw: string): DiffLine[] {
  const lines = raw.split(/\r?\n/);
  const out: DiffLine[] = [];
  for (const line of lines) {
    if (line.startsWith("+++") || line.startsWith("---")) {
      out.push({ tag: "meta", text: line });
    } else if (line.startsWith("@@")) {
      out.push({ tag: "header", text: line });
    } else if (line.startsWith("+")) {
      out.push({ tag: "add", text: line });
    } else if (line.startsWith("-")) {
      out.push({ tag: "del", text: line });
    } else {
      out.push({ tag: "ctx", text: line });
    }
  }
  return out;
}

const SAMPLE_DIFF = `--- a/src/foo.ts
+++ b/src/foo.ts
@@ -1,5 +1,6 @@
 import React from "react";
+import { useState } from "react";
 
 function App() {
-  return <div>Hello</div>;
+  return <div>World</div>;
+}
 }`;

describe("parseUnifiedDiff", () => {
  it("parses meta lines for --- and +++", () => {
    const lines = parseUnifiedDiff(SAMPLE_DIFF);
    const metas = lines.filter((l) => l.tag === "meta");
    expect(metas).toHaveLength(2);
    expect(metas[0].text).toBe("--- a/src/foo.ts");
    expect(metas[1].text).toBe("+++ b/src/foo.ts");
  });

  it("parses hunk headers", () => {
    const lines = parseUnifiedDiff(SAMPLE_DIFF);
    const headers = lines.filter((l) => l.tag === "header");
    expect(headers).toHaveLength(1);
    expect(headers[0].text).toMatch(/^@@/);
  });

  it("counts added and deleted lines correctly", () => {
    const lines = parseUnifiedDiff(SAMPLE_DIFF);
    const adds = lines.filter((l) => l.tag === "add");
    const dels = lines.filter((l) => l.tag === "del");
    expect(adds).toHaveLength(3);
    expect(dels).toHaveLength(1);
  });

  it("identifies context lines", () => {
    const lines = parseUnifiedDiff(SAMPLE_DIFF);
    const ctx = lines.filter((l) => l.tag === "ctx");
    expect(ctx.length).toBeGreaterThan(0);
    expect(ctx.some((l) => l.text === ' import React from "react";')).toBe(true);
  });

  it("handles empty diff", () => {
    const lines = parseUnifiedDiff("");
    expect(lines).toHaveLength(1); // one empty string line from split
    expect(lines[0].tag).toBe("ctx");
  });

  it("handles CRLF line endings", () => {
    const crlfDiff = "+++ b/file.ts\r\n--- a/file.ts\r\n@@ -1 +1 @@\r\n+new\r\n-old";
    const lines = parseUnifiedDiff(crlfDiff);
    expect(lines.filter((l) => l.tag === "meta")).toHaveLength(2);
    expect(lines.filter((l) => l.tag === "add")).toHaveLength(1);
    expect(lines.filter((l) => l.tag === "del")).toHaveLength(1);
  });
});
