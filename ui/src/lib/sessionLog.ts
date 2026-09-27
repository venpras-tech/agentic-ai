import { api, type SessionAppendRecord } from "./ipc";

/** Generate a turn id that makes `session_append` idempotent across replays. */
export function newTurnId(): string {
  const c = globalThis.crypto;
  if (c && typeof c.randomUUID === "function") return c.randomUUID();
  return `t-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/**
 * Turn id for the assistant half of a turn.
 *
 * The backend de-dupes by `turnId` across the whole chat log, so the reply
 * must NOT reuse the user's id or it would be silently dropped as a replay.
 */
export function assistantTurnId(userTurnId: string): string {
  return `${userTurnId}-assistant`;
}

/** A chat id is used as a filename segment by the backend, so keep it strict. */
export function newChatId(): string {
  return `chat-${Date.now().toString(36)}`;
}

/**
 * Append one turn to the project's JSONL session log.
 *
 * Failures are swallowed on purpose: losing history must never break the
 * user's turn, and the backend already de-dupes replays by `turnId`.
 */
export async function appendTurn(
  project: string,
  record: SessionAppendRecord,
  chatId?: string | null,
): Promise<void> {
  if (!project) return;
  try {
    await api.sessionAppend(project, record, chatId ?? null);
  } catch {
    // Best-effort persistence.
  }
}

/** Split a HistoryPanel conversation id (`"<projectKey>::<chatId>"`). */
export function splitConversationId(
  id: string,
): { project: string; chatId: string } {
  const idx = id.indexOf("::");
  if (idx < 0) return { project: id, chatId: "default" };
  return { project: id.slice(0, idx), chatId: id.slice(idx + 2) || "default" };
}

/**
 * Resolve the chat id to persist into.
 *
 * A brand-new chat has no conversation yet, so the backend's shared `default`
 * log is the right destination; a history-selected chat must be targeted
 * explicitly or its turns would land in the wrong file.
 */
export function resolveChatId(currentConversationId: string | null): string | null {
  if (!currentConversationId) return null;
  const { chatId } = splitConversationId(currentConversationId);
  return chatId || null;
}
