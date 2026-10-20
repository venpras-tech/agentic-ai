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
 *
 * `onPersisted` fires only after a confirmed write, so the history list
 * refetches exactly when a new chat or a new turn actually landed.
 */
export async function appendTurn(
  project: string,
  record: SessionAppendRecord,
  chatId?: string | null,
  onPersisted?: () => void,
): Promise<void> {
  if (!project) return;
  try {
    await api.sessionAppend(project, record, chatIdForInvoke(chatId));
    onPersisted?.();
  } catch (e) {
    // Best-effort persistence — a lost turn must never break the user's run.
    // Surfaced in dev only: a silent failure here is indistinguishable from
    // "history is broken" when debugging a report.
    if (import.meta.env.DEV) {
      console.error("session_append failed", { project, chatId, error: e });
    }
  }
}

/**
 * The default chat's sentinel in a conversation id.
 *
 * The backend stores it as the flat `sessions/<key>.jsonl`, addressed by a
 * `null` chat id — there is no chat file literally named `default`. Keeping the
 * marker in the UI id lets one string represent both layouts.
 */
const DEFAULT_CHAT = "default";

/** Split a HistoryPanel conversation id (`"<projectKey>::<chatId>"`). */
export function splitConversationId(
  id: string,
): { project: string; chatId: string } {
  const idx = id.indexOf("::");
  if (idx < 0) return { project: id, chatId: DEFAULT_CHAT };
  return { project: id.slice(0, idx), chatId: id.slice(idx + 2) || DEFAULT_CHAT };
}

/**
 * The chat id to send the backend, or `null` for the default chat.
 *
 * `session_file` treats a `Some(id)` as a named chat under
 * `sessions/<key>/<chat>.jsonl` and `None` as the legacy flat
 * `sessions/<key>.jsonl`. Passing the sentinel through verbatim would send
 * reads and appends to a `<key>/default.jsonl` that nothing ever writes, so the
 * default chat has to collapse back to `null` at the IPC boundary.
 */
export function chatIdForInvoke(chatId: string | null | undefined): string | null {
  if (!chatId || chatId === DEFAULT_CHAT) return null;
  return chatId;
}

/** Is this conversation id the project's base (default) chat? */
export function isDefaultChat(conversationId: string): boolean {
  return splitConversationId(conversationId).chatId === DEFAULT_CHAT;
}

/**
 * Resolve the chat id to persist into.
 *
 * A brand-new chat has no conversation yet, so the backend's shared default
 * log is the right destination; a history-selected chat must be targeted
 * explicitly or its turns would land in the wrong file.
 */
export function resolveChatId(currentConversationId: string | null): string | null {
  if (!currentConversationId) return null;
  const { chatId } = splitConversationId(currentConversationId);
  return chatIdForInvoke(chatId);
}
