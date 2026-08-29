import { describe, expect, it } from "vitest";

import { recordsToMessages } from "./session";

describe("recordsToMessages", () => {
  it("round-trips user/assistant/error records with done stats", () => {
    const msgs = recordsToMessages([
      { role: "user", content: "hi", ts: 1 },
      {
        role: "assistant",
        content: "hello",
        ts: 2,
        done: {
          totalTokens: 10,
          generatedChars: 5,
          tokensPerSec: 12.5,
          elapsedMs: 800,
          stopReason: "eos",
          outcome: "interrupted",
          inputTokens: 4,
          outputTokens: 6,
          cacheReadTokens: 0,
          cacheWriteTokens: 4,
          reasoningTokens: 0,
        },
      },
      { role: "error", content: "⚠ boom" },
    ]);
    expect(msgs).toHaveLength(3);
    expect(msgs[0]).toEqual({ role: "user", content: "hi", ts: 1 });
    expect(msgs[1].role).toBe("assistant");
    expect(msgs[1].done?.outcome).toBe("interrupted");
    expect(msgs[1].done?.elapsedMs).toBe(800);
    expect(msgs[2].role).toBe("error");
  });

  it("skips malformed records and non-chat roles", () => {
    const msgs = recordsToMessages([
      { content: "no role" },
      { role: 42, content: "bad role type" },
      { role: "assistant" }, // no content at all
      { role: "system", content: "internal record" },
      { role: "tool", content: "feedback record" },
      { role: "assistant", content: "" },
    ] as never[]);
    // The empty-body assistant is kept (legacy) as "…"; everything else dropped.
    expect(msgs).toEqual([{ role: "assistant", content: "…" }]);
  });

  it("returns an empty conversation for empty or garbage input", () => {
    expect(recordsToMessages([])).toEqual([]);
    expect(recordsToMessages([null as never, "x" as never, 3 as never])).toEqual([]);
  });

  it("tolerates partial done objects without crashing", () => {
    const msgs = recordsToMessages([{ role: "assistant", content: "c", done: {} }]);
    expect(msgs[0].done?.outcome).toBeUndefined();
    expect(msgs[0].done?.totalTokens).toBeUndefined();
  });
});
