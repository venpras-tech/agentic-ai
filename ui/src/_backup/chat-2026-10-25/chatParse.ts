export type ChatSegment =
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | {
      kind: "tool";
      name: string;
      status: string;
      text: string;
      /** Wall-clock tool duration in ms, when the backend reported it. */
      durationMs?: number;
      /** Process exit code for shell tools, when reported. */
      exitCode?: number;
      /** Raw arguments/detail string, when reported. */
      args?: string;
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
        text: inner,
        durationMs: Number.isFinite(durationMs) ? durationMs : undefined,
        exitCode: Number.isFinite(exitCode) ? exitCode : undefined,
        args: attrs.args || attrs.detail || undefined,
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

export function stripChatTags(content: string): string {
  return content
    .replace(STRIP_RE, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}