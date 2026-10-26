export type ChatSegment =
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | {
      kind: "tool";
      name: string;
      status: string;
      text: string;
      durationMs?: number;
      exitCode?: number;
      args?: string;
      callId?: string;
    }
  | { kind: "diff"; path: string; text: string }
  | { kind: "gate"; reason: string; text: string };

const TAG_NAMES = "agent_thought|tool_call|hitl_gate|diff";

function parseAttrs(raw: string): Record<string, string> {
  const out: Record<string, string> = {};
  const re = /(\w[\w-]*)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(raw))) out[m[1].toLowerCase()] = m[2] ?? m[3] ?? m[4] ?? "";
  return out;
}

const SEGMENT_RE = new RegExp(
  `<(${TAG_NAMES})([\\s\\S]*?)>([\\s\\S]*?)(?:<\\/\\1>|$)`,
  "g",
);

const CACHE_MAX = 256;
const cache = new Map<string, ChatSegment[]>();

export function parseChatSegments(content: string): ChatSegment[] {
  const hit = cache.get(content);
  if (hit) return hit;
  const segments = parseChatSegmentsUncached(content);
  cache.set(content, segments);
  if (cache.size > CACHE_MAX) {
    const oldest = cache.keys().next().value;
    if (oldest !== undefined) cache.delete(oldest);
  }
  return segments;
}

function parseChatSegmentsUncached(content: string): ChatSegment[] {
  const segments: ChatSegment[] = [];
  let cursor = 0;
  for (const m of content.matchAll(SEGMENT_RE)) {
    const index = m.index ?? 0;
    if (index > cursor) {
      segments.push({ kind: "text", text: content.slice(cursor, index) });
    }
    const tag = m[1];
    const attrs = parseAttrs(m[2] ?? "");
    const inner = (m[3] ?? "").trim();
    cursor = index + m[0].length;
    if (tag === "agent_thought") {
      segments.push({ kind: "reasoning", text: inner });
    } else if (tag === "tool_call") {
      const dur = attrs.durationms ?? attrs.duration ?? attrs.ms;
      const exit = attrs.exitcode ?? attrs.exit;
      const durationMs = dur != null && dur !== "" ? Number(dur) : undefined;
      const exitCode = exit != null && exit !== "" ? Number(exit) : undefined;
      segments.push({
        kind: "tool",
        name: attrs.name || "tool",
        status: attrs.status || (inner ? "success" : "running"),
        text: inner || attrs.detail || "",
        durationMs: Number.isFinite(durationMs) ? durationMs : undefined,
        exitCode: Number.isFinite(exitCode) ? exitCode : undefined,
        args: attrs.args || attrs.detail || undefined,
        callId: attrs.callid || undefined,
      });
    } else if (tag === "diff") {
      segments.push({ kind: "diff", path: attrs.path || "", text: inner });
    } else {
      segments.push({ kind: "gate", reason: attrs.reason || inner, text: inner });
    }
  }
  if (cursor < content.length) {
    segments.push({ kind: "text", text: content.slice(cursor) });
  }
  return segments;
}

// Closed tags strip wholesale; an unclosed trailing tag strips to end of
// stream (streaming tail cleanup).
const STRIP_RE = new RegExp(
  `<(${TAG_NAMES})[\\s\\S]*?<\\/\\1>|<(${TAG_NAMES})[\\s\\S]*$`,
  "g",
);

const EXECUTE_TOOL_RE =
  /<execute_tool>[\s\S]*?<\/execute_tool>|<execute_tool>[\s\S]*$/gi;

export function stripExecuteToolTags(content: string): string {
  return content
    .replace(EXECUTE_TOOL_RE, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function stripChatTags(content: string): string {
  return stripExecuteToolTags(
    content
      .replace(STRIP_RE, "")
      .replace(/\n{3,}/g, "\n\n")
      .trim(),
  );
}

function escAttr(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/"/g, "&quot;");
}

function escBody(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;");
}

export function formatToolCallTag(input: {
  id: string;
  tool: string;
  status: string;
  summary?: string;
  output?: string;
  detail?: string;
  durationMs?: number;
  exitCode?: number;
}): string {
  const status =
    input.status === "done"
      ? "success"
      : input.status === "error"
        ? "error"
        : input.status;
  const body =
    (input.output || input.detail || "").trim() ||
    (status === "error" ? input.detail || input.summary || "Tool failed" : input.summary || "");
  const attrs = [
    `name="${escAttr(input.tool)}"`,
    `status="${escAttr(status)}"`,
    `callId="${escAttr(input.id)}"`,
  ];
  if (input.durationMs != null) attrs.push(`durationMs="${input.durationMs}"`);
  if (input.exitCode != null) attrs.push(`exitCode="${input.exitCode}"`);
  if (input.detail) attrs.push(`detail="${escAttr(input.detail)}"`);
  return `<tool_call ${attrs.join(" ")}>${escBody(body)}</tool_call>`;
}

const TOOL_CALL_BY_ID_RE = (id: string) =>
  new RegExp(`<tool_call\\b[^>]*callId="${id.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}"[^>]*>[\\s\\S]*?</tool_call>`, "g");

export function upsertToolCallInContent(
  content: string,
  tag: string,
  callId: string,
): string {
  const base = stripExecuteToolTags(content);
  const re = TOOL_CALL_BY_ID_RE(callId);
  if (re.test(base)) {
    return base.replace(re, tag);
  }
  return base ? `${base}\n\n${tag}` : tag;
}