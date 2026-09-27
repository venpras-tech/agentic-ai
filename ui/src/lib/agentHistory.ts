import { api, type SessionRecord } from "./ipc";

/** Coerce a persisted record into a UI chat message, dropping empties. */
export function recordToMessage(
  record: SessionRecord,
): { role: "user" | "assistant" | "error"; content: string; ts?: number; done?: Record<string, unknown> } | null {
  const content = (record.content ?? "").toString();
  if (!content.trim()) return null;
  const rawRole = (record.role ?? "assistant").toString();
  const role: "user" | "assistant" | "error" =
    rawRole === "user" ? "user" : rawRole === "error" ? "error" : "assistant";
  return {
    role,
    content,
    ts: typeof record.ts === "number" ? record.ts : undefined,
    done: record.done,
  };
}

/**
 * Replay a stored conversation into the backend `ContextManager` so a
 * reloaded chat genuinely remembers prior turns.
 *
 * The manager exposes no "clear" command, so the caller should invoke this
 * right after switching conversations; the sliding-window trimmer keeps the
 * replay bounded once the token limit is crossed.
 */
export async function hydrateContextFromSession(
  records: SessionRecord[],
  systemPrompt: string,
): Promise<void> {
  await api.contextSetSystemPrompt(systemPrompt);
  for (const record of records) {
    const content = (record.content ?? "").toString();
    if (!content.trim()) continue;
    const role = (record.role ?? "assistant").toString() === "user" ? "user" : "assistant";
    await api.contextPushTurn(role, content).catch(() => {});
  }
}
