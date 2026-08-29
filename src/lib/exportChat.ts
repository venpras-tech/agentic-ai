import type { ChatMessage } from "../types";
import { isTauriRuntime, tauriInvoke } from "./ipc";

/** Supported export targets. */
export type ExportFormat = "pdf" | "docx" | "csv";

const FORMATS: readonly ExportFormat[] = ["pdf", "docx", "csv"];

/** Raised with a user-presentable message when an export cannot proceed. */
export class ExportError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ExportError";
  }
}

/** Validate + normalize the requested format. */
export function parseExportFormat(raw: string): ExportFormat {
  const f = raw.toLowerCase().trim();
  if (!FORMATS.includes(f as ExportFormat)) {
    throw new ExportError(`Unsupported export format "${raw}" — use pdf, docx or csv.`);
  }
  return f as ExportFormat;
}

function requireMessages(
  messages: Pick<ChatMessage, "role" | "content">[],
): void {
  if (!Array.isArray(messages)) throw new ExportError("No conversation to export.");
  const usable = messages.filter((m) => typeof m.content === "string");
  if (usable.length === 0) {
    throw new ExportError("This chat is empty — nothing to export yet.");
  }
}

/** Filesystem-safe base name for the exported file. */
export function exportBasename(title: string): string {
  const stamp = new Date()
    .toISOString()
    .slice(0, 16)
    .replaceAll("T", "-")
    .replaceAll(":", "");
  const safe = (title || "chat")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 40);
  return `${safe || "chat"}-${stamp}`;
}

// ---------------------------------------------------------------------------
// CSV
// ---------------------------------------------------------------------------

function csvCell(value: string): string {
  // RFC 4180: quote cells containing quotes/commas/newlines, double the quotes.
  if (/[",\n\r]/.test(value)) return `"${value.replaceAll('"', '""')}"`;
  return value;
}

/**
 * Build an RFC 4180 CSV document (UTF-8 with BOM so Excel renders Unicode
 * correctly). One row per message: timestamp (ISO or empty), role, content.
 */
export function toCsv(
  messages: Pick<ChatMessage, "role" | "content" | "ts">[],
): string {
  requireMessages(messages);
  const rows: string[] = ["timestamp,role,content"];
  for (const m of messages) {
    const ts =
      typeof m.ts === "number" && Number.isFinite(m.ts)
        ? new Date(m.ts).toISOString()
        : "";
    rows.push([csvCell(ts), csvCell(m.role), csvCell(m.content)].join(","));
  }
  // BOM first so Excel detects UTF-8 for CJK/emoji content.
  return `\uFEFF${rows.join("\r\n")}\r\n`;
}

// ---------------------------------------------------------------------------
// Shared block model for PDF / DOCX rendering
// ---------------------------------------------------------------------------

type TurnBlock = { role: string; body: string[] };

function toBlocks(
  messages: Pick<ChatMessage, "role" | "content">[],
): TurnBlock[] {
  requireMessages(messages);
  return messages.map((m) => ({
    role: m.role.toUpperCase(),
    // Split on hard newlines so long paragraphs still wrap naturally.
    body: m.content.split(/\r?\n/),
  }));
}

function exportTitle(title: string): string {
  return `AI Editor — ${title} — ${new Date().toLocaleString()}`;
}

// ---------------------------------------------------------------------------
// DOCX
// ---------------------------------------------------------------------------

/** Build a .docx document blob (dynamic import keeps it out of the main chunk). */
export async function toDocx(
  messages: Pick<ChatMessage, "role" | "content">[],
  title: string,
): Promise<Blob> {
  const blocks = toBlocks(messages);
  const { Document, Packer, Paragraph, TextRun, HeadingLevel } = await import("docx");

  const children: InstanceType<typeof Paragraph>[] = [
    new Paragraph({
      text: exportTitle(title),
      heading: HeadingLevel.HEADING_2,
    }),
  ];
  let lastRole: string | null = null;
  for (const block of blocks) {
    if (block.role !== lastRole) {
      children.push(
        new Paragraph({
          children: [
            new TextRun({
              text: block.role,
              bold: true,
              color: block.role === "ERROR" ? "C0392B" : "52525B",
              size: 18,
            }),
          ],
          spacing: { before: 200 },
        }),
      );
      lastRole = block.role;
    }
    for (const line of block.body.length > 0 ? block.body : [""]) {
      children.push(new Paragraph({ children: [new TextRun(line)] }));
    }
  }

  const doc = new Document({ sections: [{ children }] });
  return Packer.toBlob(doc);
}

// ---------------------------------------------------------------------------
// PDF
// ---------------------------------------------------------------------------

/** Build a paginated A4 PDF blob (dynamic-imported jsPDF, lazy-loaded). */
export async function toPdf(
  messages: Pick<ChatMessage, "role" | "content">[],
  title: string,
): Promise<Blob> {
  const blocks = toBlocks(messages);
  const { jsPDF } = await import("jspdf");
  const doc = new jsPDF({ unit: "pt", format: "a4" });

  const pageW = doc.internal.pageSize.getWidth();
  const pageH = doc.internal.pageSize.getHeight();
  const margin = 48;
  const wrapW = pageW - margin * 2;
  let y = margin;

  const ensureSpace = (needed: number) => {
    if (y + needed <= pageH - margin) return;
    doc.addPage();
    y = margin;
  };

  doc.setFont("helvetica", "bold");
  doc.setFontSize(13);
  for (const line of doc.splitTextToSize(exportTitle(title), wrapW)) {
    ensureSpace(18);
    doc.text(line, margin, y);
    y += 18;
  }
  y += 8;

  for (const block of blocks) {
    ensureSpace(24);
    doc.setFont("helvetica", "bold");
    doc.setFontSize(9);
    doc.setTextColor(block.role === "ERROR" ? 192 : 82, block.role === "ERROR" ? 57 : 82, block.role === "ERROR" ? 43 : 91);
    doc.text(block.role, margin, y);
    y += 14;

    doc.setFont("helvetica", "normal");
    doc.setFontSize(10.5);
    doc.setTextColor(24, 24, 27);
    const lines =
      block.body.length > 0
        ? block.body.flatMap((l) => doc.splitTextToSize(l, wrapW))
        : [""];
    for (const line of lines) {
      ensureSpace(15);
      doc.text(line, margin, y);
      y += 15;
    }
    y += 6;
  }

  return doc.output("blob");
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

async function saveBlob(blob: Blob, filename: string): Promise<void> {
  if (isTauriRuntime()) {
    const arrayBuf = await blob.arrayBuffer();
    const bytes = new Uint8Array(arrayBuf);
    let binary = "";
    for (let i = 0; i < bytes.byteLength; i++) {
      binary += String.fromCharCode(bytes[i]);
    }
    const base64 = btoa(binary);
    await tauriInvoke<string | null>("save_file_as_bytes", {
      content: base64,
      suggestedFilename: filename,
    });
    return;
  }
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 5_000);
}

/**
 * Export one conversation. Validates input up front, lazy-loads the matching
 * generator, and triggers a browser download. Throws {@link ExportError} with
 * a user-presentable message on any failure.
 */
export async function exportConversation(opts: {
  messages: ChatMessage[];
  title: string;
  format: string;
}): Promise<string> {
  const format = parseExportFormat(opts.format);
  const basename = exportBasename(opts.title);
  try {
    switch (format) {
      case "csv":
        await saveBlob(new Blob([toCsv(opts.messages)], { type: "text/csv;charset=utf-8" }), `${basename}.csv`);
        break;
      case "docx":
        await saveBlob(await toDocx(opts.messages, opts.title), `${basename}.docx`);
        break;
      case "pdf":
        await saveBlob(await toPdf(opts.messages, opts.title), `${basename}.pdf`);
        break;
    }
    return `${basename}.${format}`;
  } catch (e) {
    if (e instanceof ExportError) throw e;
    throw new ExportError(`Export failed: ${String(e)}`);
  }
}
