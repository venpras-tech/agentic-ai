export type ChatSegment =
  | { kind: "text"; text: string }
  | { kind: "reasoning"; text: string }
  | { kind: "tool"; name: string; status: string; text: string }
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

export function parseChatSegments(content: string): ChatSegment[] {
  const segments: ChatSegment[] = [];
  let cursor = 0;
  let m: RegExpExecArray | null;
  while ((m = SEGMENT_RE.exec(content))) {
    if (m.index > cursor) {
      segments.push({ kind: "text", text: content.slice(cursor, m.index) });
    }
    const tag = m[1];
    const attrs = parseAttrs(m[2] ?? "");
    const inner = (m[3] ?? "").trim();
    cursor = m.index + m[0].length;
    if (tag === "agent_thought") {
      segments.push({ kind: "reasoning", text: inner });
    } else if (tag === "tool_call") {
      segments.push({
        kind: "tool",
        name: attrs.name || "tool",
        status: attrs.status || (inner ? "success" : "running"),
        text: inner,
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