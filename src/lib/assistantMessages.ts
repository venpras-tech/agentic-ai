import type {
  AppendMessage,
  ExternalStoreMessageConverter,
  ThreadMessageLike,
} from "@assistant-ui/react";
import type { ChatMessage } from "../types";

/**
 * Adapter record we feed into the assistant-ui external-store runtime. It
 * mirrors the app's canonical transcript (`messages`) while the assistant text
 * streams in from `streams` (keyed by agent session id), so the runtime sees
 * the same text the UI renders. The raw `ChatMessage` rides along inside
 * `internal` for anything that wants the full turn payload.
 */
export type ExternalChatMessage = {
  readonly id: string;
  readonly role: "user" | "assistant";
  readonly content: string;
  internal: ChatMessage;
};

export const convertExternalMessage: ExternalStoreMessageConverter<ExternalChatMessage> = (
  message,
): ThreadMessageLike => ({
  id: message.id,
  role: message.role,
  content: [{ type: "text", text: message.content }],
  metadata: { custom: { internal: message.internal } },
});

/**
 * Build the runtime's message snapshot from the canonical transcript. Error
 * turns map to the assistant role (the app styles them via `internal.role`).
 * Assistant turns whose session is still streaming take their live text from
 * `streams` so the external store keeps updating as tokens arrive.
 */
export function toExternalMessages(
  messages: readonly ChatMessage[],
  streams: ReadonlyMap<number, string>,
): ExternalChatMessage[] {
  const out: ExternalChatMessage[] = new Array(messages.length);
  for (let i = 0; i < messages.length; i++) {
    const m = messages[i];
    const role: ExternalChatMessage["role"] = m.role === "user" ? "user" : "assistant";
    const live = role === "assistant" && m.sessionId != null && streams.has(m.sessionId);
    const content =
      live && m.sessionId != null
        ? (streams.get(m.sessionId) ?? m.content)
        : m.content;
    out[i] = {
      // Both halves of a turn share the caller's turnId; the role suffix
      // keeps the ids distinct and stable across renders (id-based fallback
      // for legacy messages without a turnId).
      id: `${m.turnId ?? `msg:${i}`}:${role}`,
      role,
      content,
      internal: m,
    };
  }
  return out;
}

/** Extract the plain-text payload of an `AppendMessage` (composer sends). */
export function getAppendText(message: AppendMessage): string {
  const content = message.content;
  if (typeof content === "string") return content;
  if (!Array.isArray(content)) return "";
  let text = "";
  for (const part of content) {
    if (part && part.type === "text" && typeof part.text === "string") text += part.text;
  }
  return text;
}