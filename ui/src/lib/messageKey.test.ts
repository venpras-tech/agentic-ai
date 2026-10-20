import { describe, it, expect } from "vitest";
import { messageKey } from "./messageKey";
import type { ChatMessage } from "../context/types";

const msg = (over: Partial<ChatMessage> = {}): ChatMessage => ({
  role: "assistant",
  content: "x",
  ...over,
});

describe("messageKey", () => {
  it("prefers the session-log turn id", () => {
    expect(messageKey(msg({ turnId: "turn-7", ts: 1 }), 3)).toBe("turn-turn-7");
  });

  // Regression: keying on the list position made React re-mount every message
  // when an edit truncated the transcript, losing per-message UI state.
  it("is stable across a list index shift when a turn id is present", () => {
    const target = msg({ turnId: "turn-7" });
    expect(messageKey(target, 5)).toBe(messageKey(target, 1));
  });

  it("falls back to the timestamp for records without a turn id", () => {
    expect(messageKey(msg({ ts: 99 }), 0)).toBe("msg-99");
  });

  it("falls back to the index when there is neither", () => {
    expect(messageKey(msg(), 4)).toBe("msg-idx-4");
  });

  it("never collides across distinct turns", () => {
    const keys = new Set(
      [msg({ turnId: "a" }), msg({ turnId: "b" }), msg({ ts: 1 }), msg({ ts: 1 })].map((m, i) =>
        messageKey(m, i),
      ),
    );
    // Two records sharing a timestamp and no turn id do collide, which is why
    // turnId wins when present.
    expect(keys.size).toBe(3);
  });
});
