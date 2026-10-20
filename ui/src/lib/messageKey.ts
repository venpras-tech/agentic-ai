import type { ChatMessage } from "../context/types";

/**
 * Stable React key for a chat message.
 *
 * `session_append` dedupes by `turnId`, so the id is already the identity the
 * rest of the app uses — preferring it keeps a message mounted across the
 * re-render triggered by every streamed token. Records that predate
 * `turnId` (or were written by a turn that never got one) fall back to the
 * timestamp, then to the list position as a last resort.
 */
export function messageKey(message: ChatMessage, index: number): string {
  if (message.turnId) return `turn-${message.turnId}`;
  if (message.ts != null) return `msg-${message.ts}`;
  return `msg-idx-${index}`;
}
