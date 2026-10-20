import { describe, it, expect } from "vitest";
import {
  assistantTurnId,
  chatIdForInvoke,
  isDefaultChat,
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

  // Regression: the base chat lives at the flat `sessions/<key>.jsonl`, which
  // the backend addresses with a null chat id. Forwarding the "default"
  // sentinel verbatim addressed `sessions/<key>/default.jsonl` — a file nothing
  // writes — so history loads came back empty and new turns went to a phantom
  // log that surfaced later as a duplicate entry.
  it("collapses the default sentinel to null at the IPC boundary", () => {
    expect(chatIdForInvoke("default")).toBeNull();
    expect(chatIdForInvoke(null)).toBeNull();
    expect(chatIdForInvoke(undefined)).toBeNull();
    expect(chatIdForInvoke("")).toBeNull();
    // A named chat must survive untouched.
    expect(chatIdForInvoke("chat-abc")).toBe("chat-abc");
  });

  it("never resolves a default conversation to a named chat id", () => {
    expect(resolveChatId("myproj::default")).toBeNull();
    expect(resolveChatId("myproj")).toBeNull();
  });

  it("identifies the base chat regardless of how the id was formed", () => {
    expect(isDefaultChat("myproj::default")).toBe(true);
    expect(isDefaultChat("myproj")).toBe(true);
    expect(isDefaultChat("myproj::")).toBe(true);
    expect(isDefaultChat("myproj::chat-abc")).toBe(false);
  });
});
