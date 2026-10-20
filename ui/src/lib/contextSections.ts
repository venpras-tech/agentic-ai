import { api, type ImageAttachment } from "./ipc";
import type { ActiveFileBuffer, ContextState } from "../context/types";

/** Per-file / per-section character caps so a handful of chips can never
 *  blow the model's context window on their own. */
const MAX_FILES = 8;
const MAX_FILE_CHARS = 12_000;
const MAX_SEGMENT_CHARS = 8_000;
const MAX_TREE_ENTRIES = 200;
const MAX_PAST_CHAT_RECORDS = 12;
const MAX_PAST_CHAT_CHARS = 4_000;
const MAX_DOC_URLS = 10;

export interface BuiltContext {
  /** Appended to the pinned system prompt: directives and references. */
  promptSections: string[];
  /** Pinned file-buffer text: the literal file/code contents. */
  buffer: string;
  /** Vision attachments for providers that accept images. */
  images: ImageAttachment[];
}

export interface BuildContextOptions {
  contextPaths: string[];
  contextState: ContextState;
  projectPath: string;
  activeFile: ActiveFileBuffer | null;
  /** Chats other than the active one, newest first, for @Past Chats. */
  pastChats?: Array<{ id: string; title: string }>;
}

function clip(text: string, max: number): string {
  if (text.length <= max) return text;
  return `${text.slice(0, max)}\n… (truncated, ${text.length - max} more chars)`;
}

/** Extract the numbered `start..end` (1-based, inclusive) lines of `content`,
 *  with per-line numbers so the agent can reference exact locations. Unknown
 *  boundaries clamp to the file; a start past the end yields an empty string. */
export function lineRange(content: string, start?: number, end?: number): string {
  if (content === "") return "";
  const lines = content.split(/\r?\n/);
  const from = Math.max(1, start ?? 1);
  const to = Math.min(lines.length, end ?? lines.length);
  if (from > lines.length) return "";
  const selected = lines.slice(from - 1, to);
  if (selected.length === 0) return "";
  const width = String(to).length;
  return selected
    .map((line, i) => `${String(from + i).padStart(width, " ")} | ${line}`)
    .join("\n");
}

/** Normalize an image reference into the `{ dataUrl }` shape the backend reads. */
export function parseDataUrl(value: string): ImageAttachment | null {
  const v = value.trim();
  if (!v) return null;
  if (!/^data:[^;,]+;base64,/s.test(v)) return null;
  return { dataUrl: v };
}

/** A compact top-level view of the workspace, so @Codebase gives the model
 *  real orientation instead of just a flag it cannot act on. */
async function buildWorkspaceTree(projectPath: string): Promise<string> {
  const entries = await api.listDirectory(projectPath, null).catch(() => null);
  if (!entries || entries.length === 0) return "";
  const lines = entries
    .slice(0, MAX_TREE_ENTRIES)
    .map((e) => (e.isDir ? `${e.name}/` : e.name));
  const suffix =
    entries.length > MAX_TREE_ENTRIES ? `\n… (${entries.length - MAX_TREE_ENTRIES} more entries)` : "";
  return `${projectPath}\n${lines.join("\n")}${suffix}`;
}

/**
 * Turn the user's context chips into text the agent can actually use.
 *
 * Two channels come back: `promptSections` for things the model should *know
 * about* (references, tool directives) and `buffer` for literal file text,
 * which is pinned in the backend's never-evicted file buffer.
 */
export async function buildContext(opts: BuildContextOptions): Promise<BuiltContext> {
  const promptSections: string[] = [];
  const bufferParts: string[] = [];
  const images: ImageAttachment[] = [];
  const { contextPaths, contextState, projectPath, activeFile } = opts;

  // ---- active editor buffer (pinned, never evicted) ----
  if (activeFile && activeFile.content.trim()) {
    bufferParts.push(
      `### Active file: ${activeFile.path}\n${clip(activeFile.content, MAX_FILE_CHARS)}`,
    );
  }

  // ---- attached file paths: index for RAG + inline excerpt ----
  for (const path of contextPaths.slice(0, MAX_FILES)) {
    try {
      // Index the file so searchAttachedFiles / semanticSearchCodebase can
      // retrieve whole-bleed passages, not just the excerpt below.
      await api.agentAttachFile(path);
    } catch {
      // Unreadable or binary — the inline read below reports the real error.
    }
    try {
      const file = await api.readTextFile(path);
      bufferParts.push(`### ${path}\n${clip(file.content, MAX_FILE_CHARS)}`);
    } catch {
      // Skip paths that cannot be read as text.
    }
  }

  // ---- explicit code selections ----
  for (const seg of contextState.code) {
    try {
      const file = await api.readTextFile(seg.path);
      const range = seg.startLine
        ? lineRange(file.content, seg.startLine, seg.endLine)
        : file.content;
      const label = seg.startLine
        ? `${seg.path}:${seg.startLine}${seg.endLine && seg.endLine !== seg.startLine ? `-${seg.endLine}` : ""}`
        : seg.path;
      if (range.trim()) {
        bufferParts.push(`### ${label}\n${clip(range, MAX_SEGMENT_CHARS)}`);
      }
    } catch {
      // Skip unreadable segments.
    }
  }

  // ---- @Codebase ----
  if (contextState.codebase && projectPath) {
    const tree = await buildWorkspaceTree(projectPath);
    if (tree) bufferParts.push(`### Workspace layout\n${tree}`);
    promptSections.push(
      [
        "## Attached context: @Codebase",
        `The workspace root is \`${projectPath}\`. Its top level is in the context buffer.`,
        "Explore with viewFileStructure, globSearchCodebase, searchFileContents or semanticSearchCodebase",
        "rather than guessing paths. Read a file before you edit it.",
      ].join("\n"),
    );
  }

  // ---- @Git ----
  if (contextState.git) {
    const mode = contextState.git.mode || "status";
    const verbs: Record<string, string> = {
      status: "Start with gitStatus, then gitDiff for the files you intend to change.",
      diff: "Run gitDiff to see uncommitted changes before editing.",
      log: "Run gitStatus and gitDiff to understand the current branch state.",
      branch: "Run gitStatus and gitCiStatus to understand the current branch.",
    };
    promptSections.push(
      [
        "## Attached context: @Git",
        `Git context requested (mode: ${mode}).`,
        verbs[mode] || verbs.status,
        "Do not commit, push, or create branches unless the user explicitly asked.",
      ].join("\n"),
    );
  }

  // ---- @Web ----
  if (contextState.web.trim()) {
    promptSections.push(
      [
        "## Attached context: @Web",
        `The user pointed at: ${contextState.web.trim()}`,
        "Fetch it with webExtract before answering about it; use webSearch for anything it does not cover.",
      ].join("\n"),
    );
  }

  // ---- @Browser ----
  if (contextState.browser) {
    promptSections.push(
      [
        "## Attached context: @Browser",
        "Live web access is expected for this turn: use webSearch for discovery and webExtract to read pages.",
      ].join("\n"),
    );
  }

  // ---- docs (references only; no fetch backend is implied) ----
  const docs = contextState.docs.filter((d) => d.trim()).slice(0, MAX_DOC_URLS);
  if (docs.length > 0) {
    promptSections.push(
      [
        "## Attached context: docs",
        "The user supplied these references:",
        ...docs.map((d) => `- ${d}`),
        "Treat them as the source of truth for the task where they apply.",
      ].join("\n"),
    );
  }

  // ---- @Past Chats ----
  if (contextState.past_chats) {
    const records = await api
      .sessionTail(projectPath, null, MAX_PAST_CHAT_RECORDS)
      .catch(() => []);
    if (records && records.length > 0) {
      const transcript = records
        .map((r) => `${r.role ?? "?"}: ${(r.content ?? "").trim()}`)
        .filter((line) => line.length > 3)
        .join("\n");
      if (transcript) {
        bufferParts.push(
          `### Earlier conversation in this workspace\n${clip(transcript, MAX_PAST_CHAT_CHARS)}`,
        );
      }
    }
    if (opts.pastChats && opts.pastChats.length > 0) {
      promptSections.push(
        [
          "## Attached context: @Past Chats",
          "Earlier chats for this workspace:",
          ...opts.pastChats.slice(0, 10).map((c) => `- ${c.title || c.id}`),
          "Open one with the history panel if you need its full transcript.",
        ].join("\n"),
      );
    }
  }

  // ---- @Image (vision) ----
  if (contextState.visual?.image) {
    const image = parseDataUrl(contextState.visual.image);
    if (image) images.push(image);
  }

  return {
    promptSections,
    buffer: bufferParts.join("\n\n"),
    images,
  };
}
