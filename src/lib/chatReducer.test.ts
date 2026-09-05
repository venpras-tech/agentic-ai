import { describe, expect, it } from "vitest";

import type { AgentToolEvent, ChatMessage, LedgerEntry } from "../types";
import { chatReducer, emptyChat, type ChatState } from "./chatReducer";

function assistant(sessionId: number, content = ""): ChatMessage {
  return { role: "assistant", content, sessionId };
}

function tool(id: string, partial: Partial<AgentToolEvent> = {}): AgentToolEvent {
  return {
    id,
    tool: "read_file",
    status: "done",
    summary: "",
    startedAt: 0,
    sessionId: 1,
    ...partial,
  };
}

describe("chatReducer message list", () => {
  it("push appends a message", () => {
    const withStarted = chatReducer(emptyChat, {
      type: "push",
      message: assistant(5),
    });
    expect(withStarted.messages).toEqual([assistant(5)]);
  });

  it("mergeById patches only the matching session turn", () => {
    const state: ChatState = {
      messages: [assistant(1), assistant(2)],
      ledger: [],
    };
    const next = chatReducer(state, {
      type: "mergeById",
      sessionId: 2,
      patch: { content: "final", done: undefined },
    });
    expect(next.messages[0].content).toBe("");
    expect(next.messages[0].sessionId).toBe(1);
    expect(next.messages[1].content).toBe("final");
  });

  it("replaceAll swaps the list (session replay / edit fork)", () => {
    const replay = [assistant(9, "old turn")];
    const withTurn = chatReducer({ ...emptyChat, messages: [assistant(1)] }, {
      type: "replaceAll",
      messages: replay,
    });
    expect(withTurn.messages).toBe(replay);
  });

  it("clearMessages empties messages but keeps the ledger", () => {
    const seed = chatReducer(chatReducer(emptyChat, { type: "push", message: assistant(1) }), {
      type: "ledgerSet",
      entry: { sessionId: 1, label: "task", tokens: 10, toolCalls: 0, elapsedMs: 0 },
    });
    const next = chatReducer(seed, { type: "clearMessages" });
    expect(next.messages).toEqual([]);
    expect(next.ledger).toHaveLength(1);
  });

  it("reset resets both messages and ledger", () => {
    const seed = chatReducer(chatReducer(emptyChat, { type: "push", message: assistant(1) }), {
      type: "ledgerTool",
      sessionId: 1,
      label: "task",
    });
    expect(chatReducer(seed, { type: "reset" })).toEqual(emptyChat);
  });

  it("tool adds a new call and merges a duplicate by id", () => {
    const state: ChatState = {
      messages: [{ role: "user", content: "q", sessionId: 1 }],
      ledger: [],
    };
    const first = chatReducer(state, {
      type: "tool",
      sessionId: 1,
      tool: tool("a", { atChar: 0, summary: "start", status: "running" }),
    });
    expect(first.messages[0].tools).toEqual([
      tool("a", { atChar: 0, summary: "start", status: "running" }),
    ]);

    const merged = chatReducer(first, {
      type: "tool",
      sessionId: 1,
      tool: tool("a", { status: "done", atChar: 5 }),
    });
    expect(merged.messages[0].tools).toHaveLength(1);
    const t = merged.messages[0].tools![0];
    // Same id → status/anchors update, the previous in-flight output is kept.
    expect(t.status).toBe("done");
    expect(t.atChar).toBe(5);
    expect(t.output).toBeUndefined();
  });

  it("appendToolOutput merges into a running terminal tool and caps at 4000", () => {
    const state: ChatState = {
      messages: [
        {
          role: "assistant",
          content: "",
          sessionId: 1,
          tools: [tool("t", { tool: "execute_terminal_command", status: "running", output: "A" })],
        },
      ],
      ledger: [],
    };
    const next = chatReducer(state, { type: "appendToolOutput", sessionId: 1, chunk: "B" });
    expect(next.messages[0].tools![0].output).toBe("AB\n");

    const huge = chatReducer(
      {
        ...state,
        messages: [{
          role: "assistant",
          content: "",
          sessionId: 1,
          tools: [tool("t", { tool: "run_tests", status: "running", output: "x".repeat(3999) })],
        }],
      },
      { type: "appendToolOutput", sessionId: 1, chunk: "y" },
    );
    expect(huge.messages[0].tools![0].output!.length).toBe(4000);
  });

  it("appendToolOutput ignores non-terminal tools", () => {
    const state: ChatState = {
      messages: [{ role: "assistant", content: "", sessionId: 1, tools: [tool("t", { tool: "read_file", status: "running" })] }],
      ledger: [],
    };
    const next = chatReducer(state, { type: "appendToolOutput", sessionId: 1, chunk: "B" });
    expect(next.messages[0].tools![0].output).toBeUndefined();
  });

  it("appendStep and appendPlanStep build the step timeline in order", () => {
    const state: ChatState = { messages: [assistant(1)], ledger: [] };
    const stepped = chatReducer(state, {
      type: "appendStep",
      sessionId: 1,
      step: { step: 1, group: "Execute", tokens: 12, elapsedMs: 30, toolCalls: 1 },
    });
    const planned = chatReducer(stepped, { type: "appendPlanStep", sessionId: 1, group: "Plan · fix" });
    expect(planned.messages[0].steps).toEqual([
      { step: 1, group: "Execute", tokens: 12, elapsedMs: 30, toolCalls: 1 },
      { step: 2, group: "Plan · fix", tokens: 0, elapsedMs: 0, toolCalls: 0 },
    ]);
  });

  it("appendDiff appends file diffs in arrival order", () => {
    const state: ChatState = { messages: [assistant(1)], ledger: [] };
    const d1 = { path: "a.ts", kind: "diff" as const, oldText: "", newText: "x", ts: 1 };
    const d2 = { ...d1, ts: 2 };
    const next = chatReducer(state, { type: "appendDiff", sessionId: 1, diff: d2 });
    expect(next.messages[0].diffs).toEqual([d2]);
  });
});

describe("chatReducer ledger", () => {
  function seedLedger(): LedgerEntry[] {
    return [{ sessionId: 1, label: "task", tokens: 100, toolCalls: 2, elapsedMs: 10 }];
  }

  it("ledgerTool increments toolCalls and preserves the rest", () => {
    const next = chatReducer({ messages: [], ledger: seedLedger() }, {
      type: "ledgerTool",
      sessionId: 1,
      label: "task",
    });
    expect(next.ledger[0]).toMatchObject({ sessionId: 1, tokens: 100, toolCalls: 3, elapsedMs: 10 });
  });

  it("ledgerTokens accumulates tokens and preserves toolCalls", () => {
    const next = chatReducer({ messages: [], ledger: seedLedger() }, {
      type: "ledgerTokens",
      sessionId: 1,
      label: "task",
      tokens: 5,
    });
    expect(next.ledger[0]).toMatchObject({ tokens: 105, toolCalls: 2, elapsedMs: 10 });
  });

  it("ledgerSet replaces an existing session entry and appends new ones", () => {
    const next = chatReducer({ messages: [], ledger: seedLedger() }, {
      type: "ledgerSet",
      entry: { sessionId: 1, label: "task", tokens: 999, toolCalls: 4, elapsedMs: 20 },
    });
    expect(next.ledger).toHaveLength(1);
    expect(next.ledger[0].tokens).toBe(999);
  });

  it("ledger actions create entries for unseen sessions", () => {
    const next = chatReducer({ messages: [], ledger: seedLedger() }, {
      type: "ledgerTool",
      sessionId: 2,
      label: "other",
    });
    expect(next.ledger.map((l) => l.sessionId)).toEqual([1, 2]);
    expect(next.ledger[1]).toMatchObject({ sessionId: 2, toolCalls: 1, tokens: 0 });
  });
});