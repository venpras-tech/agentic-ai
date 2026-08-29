import type { ChatMessage, InferenceDone } from "../types";

/** One JSONL record as stored by the backend (`session_append`). */
export type SessionRecord = {
  role?: unknown;
  content?: unknown;
  ts?: unknown;
  done?: unknown;
};

const VALID_ROLES = new Set(["user", "assistant", "error"]);

function parseDone(raw: unknown): InferenceDone | undefined {
  if (raw == null || typeof raw !== "object") return undefined;
  const d = raw as Record<string, unknown>;
  if (typeof d.outcome !== "string") return undefined;
  return {
    totalTokens: Number(d.totalTokens ?? 0),
    generatedChars: Number(d.generatedChars ?? 0),
    tokensPerSec: Number(d.tokensPerSec ?? 0),
    elapsedMs: Number(d.elapsedMs ?? 0),
    stopReason: String(d.stopReason ?? ""),
    outcome: d.outcome as
      | "completed"
      | "failed"
      | "interrupted"
      | "error",
    inputTokens: Number(d.inputTokens ?? 0),
    outputTokens: Number(d.outputTokens ?? 0),
    cacheReadTokens: Number(d.cacheReadTokens ?? 0),
    cacheWriteTokens: Number(d.cacheWriteTokens ?? 0),
    reasoningTokens: Number(d.reasoningTokens ?? 0),
  };
}

/**
 * Map stored JSONL records back into chat messages for hydration. Malformed
 * records are skipped; assistant turns keep their lifecycle stats so restored
 * chats still show badges / per-turn metrics / stop reasons.
 */
export function recordsToMessages(records: SessionRecord[]): ChatMessage[] {
  const out: ChatMessage[] = [];
  for (const r of records) {
    if (!r || typeof r !== "object") continue;
    const { role, content } = r;
    if (typeof content !== "string" || typeof role !== "string") continue;
    if (!VALID_ROLES.has(role)) continue;
    out.push({
      role: role as ChatMessage["role"],
      // Legacy records may hold an empty assistant body — keep it visible.
      content: role === "assistant" && !content.trim() ? "…" : content,
      ...(typeof r.ts === "number" && Number.isFinite(r.ts) ? { ts: r.ts } : {}),
      ...(role !== "user" ? { done: parseDone(r.done) } : {}),
    });
  }
  return out;
}
