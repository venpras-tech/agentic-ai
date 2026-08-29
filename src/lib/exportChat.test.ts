import { describe, expect, it } from "vitest";

import {
  ExportError,
  exportBasename,
  parseExportFormat,
  toCsv,
  toDocx,
  toPdf,
} from "./exportChat";
import type { ChatMessage } from "../types";

const sample: ChatMessage[] = [
  { role: "user", content: "Hello, world!", ts: Date.UTC(2026, 0, 2, 3, 4) },
  { role: "assistant", content: "Hi! Line one.\nLine two with \"quotes\" and, commas.", ts: Date.UTC(2026, 0, 2, 3, 5), done: undefined },
];

describe("parseExportFormat", () => {
  it("accepts the three supported formats case-insensitively", () => {
    expect(parseExportFormat("PDF")).toBe("pdf");
    expect(parseExportFormat(" docx ")).toBe("docx");
    expect(parseExportFormat("csv")).toBe("csv");
  });
  it("rejects unknown formats with a helpful message", () => {
    expect(() => parseExportFormat("xlsx")).toThrow(ExportError);
    expect(() => parseExportFormat("")).toThrow(/Unsupported export format/);
  });
});

describe("toCsv", () => {
  it("writes a BOM + header + escaped RFC-4180 rows", () => {
    const csv = toCsv(sample);
    expect(csv.charCodeAt(0)).toBe(0xfeff);
    expect(csv).toContain("timestamp,role,content");
    expect(csv).toContain("2026-01-02T03:04:00.000Z,user,\"Hello, world!\"");
    // Quotes doubled, embedded newline kept inside quoted cell.
    expect(csv).toContain('""quotes"" and, commas.');
    expect(csv).toMatch(/\r\n$/);
  });
  it("keeps unicode content intact", () => {
    const csv = toCsv([{ role: "user", content: "héllo 世界 🎉" }]);
    expect(csv).toContain("héllo 世界 🎉");
  });
  it("omits timestamps for messages without ts", () => {
    const csv = toCsv([{ role: "assistant", content: "no time" }]);
    expect(csv).toContain(",assistant,no time");
  });
  it("throws on an empty conversation", () => {
    expect(() => toCsv([])).toThrow(/empty/i);
  });
  it("handles very long cells without truncation", () => {
    const long = "x".repeat(50_000);
    const csv = toCsv([{ role: "user", content: long }]);
    expect(csv.length).toBeGreaterThan(50_000);
  });
});

describe("toPdf", () => {
  it("produces bytes starting with the %PDF magic header", async () => {
    const blob = await toPdf(sample, "test chat");
    const buf = new Uint8Array(await blob.arrayBuffer());
    expect(buf.byteLength).toBeGreaterThan(500);
    expect(String.fromCharCode(...buf.slice(0, 5))).toBe("%PDF-");
  });
  it("paginates long conversations instead of failing", async () => {
    const many: ChatMessage[] = Array.from({ length: 120 }, (_, i) => ({
      role: i % 2 ? ("assistant" as const) : ("user" as const),
      content: `message ${i} — ${"lorem ipsum ".repeat(40)}`,
      ts: i,
    }));
    const blob = await toPdf(many, "long chat");
    expect(blob.size).toBeGreaterThan(10_000);
  });
});

describe("toDocx", () => {
  it("produces a non-empty OOXML (zip) blob starting with PK", async () => {
    const blob = await toDocx(sample, "doc test");
    const buf = new Uint8Array(await blob.arrayBuffer());
    expect(buf[0]).toBe("P".charCodeAt(0));
    expect(buf[1]).toBe("K".charCodeAt(0));
    expect(blob.size).toBeGreaterThan(1_000);
  });
});

describe("exportBasename", () => {
  it("sanitizes titles and appends a sortable timestamp", () => {
    const name = exportBasename("My Chat: v2?/final");
    expect(name).toMatch(/^my-chat-v2-final-\d{4}-\d{2}-\d{2}-\d{4}$/);
  });
  it("falls back to 'chat' for empty/garbage titles", () => {
    expect(exportBasename("???")).toMatch(/^chat-\d{4}-/);
  });
});
