import { describe, it, expect } from "vitest";
import {
  assistantTurnId,
  newChatId,
  newTurnId,
  resolveChatId,
  splitConversationId,
} from "./sessionLog";

describe("sessionLog", () => {
  it("generates unique turn ids", () => {
    const ids = new Set(Array.from({ length: 50 }, () => newTurnId()));
    expect(ids.size).toBe(50);
  });

  // Regression: `session_append` dedupes by turnId across the whole chat log,
  // so an assistant reply reusing the user id is silently dropped.
  it("derives an assistant turn id that differs from the user turn id", () => {
    const user = newTurnId();
    expect(assistantTurnId(user)).not.toBe(user);
  });

  it("generates chat ids safe for use as a filename segment", () => {
    expect(newChatId()).toMatch(/^chat-[a-z0-9]+$/);
  });

  it("splits a compound conversation id", () => {
    expect(splitConversationId("myproj::chat-abc")).toEqual({
      project: "myproj",
      chatId: "chat-abc",
    });
  });

  it("falls back to the default chat for a bare id", () => {
    expect(splitConversationId("myproj")).toEqual({ project: "myproj", chatId: "default" });
  });

  it("treats an empty chat segment as the default chat", () => {
    expect(splitConversationId("myproj::")).toEqual({ project: "myproj", chatId: "default" });
  });

  it("routes a brand-new chat to the shared default log", () => {
    expect(resolveChatId(null)).toBeNull();
    expect(resolveChatId("")).toBeNull();
  });

  it("routes a history-selected chat to its own log", () => {
    expect(resolveChatId("myproj::chat-abc")).toBe("chat-abc");
  });
});
