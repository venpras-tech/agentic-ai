import { describe, expect, it } from "vitest";

import { buildTurnSegments } from "../components/ChatPanel";
import type { AgentToolEvent } from "../types";

function tool(
  id: string,
  startedAt: number,
  atChar?: number,
): AgentToolEvent {
  return {
    id,
    sessionId: 1,
    tool: "read_file",
    status: "done",
    summary: `call ${id}`,
    startedAt,
    ...(atChar != null ? { atChar } : {}),
  };
}

describe("buildTurnSegments", () => {
  it("returns a single empty text segment for an empty live turn", () => {
    const { segments, unanchored } = buildTurnSegments({ content: "", tools: [] }, "");
    expect(unanchored).toEqual([]);
    expect(segments).toEqual([{ kind: "text", text: "" }]);
  });

  it("interleaves text and tools at atChar anchors in order", () => {
    const msg = {
      content: "",
      tools: [
        tool("b", 200, 20),
        tool("a", 100, 10),
      ],
    };
    const { segments, unanchored } = buildTurnSegments(msg, "0123456789ABCDEFGHIJ");
    expect(unanchored).toEqual([]);
    expect(segments).toEqual([
      { kind: "text", text: "0123456789" },
      { kind: "tool", event: tool("a", 100, 10) },
      { kind: "text", text: "ABCDEFGHIJ" },
      { kind: "tool", event: tool("b", 200, 20) },
    ]);
  });

  it("uses finalized content over live text when both exist", () => {
    const msg = { content: "final text", tools: [tool("a", 1, 5)] };
    const { segments } = buildTurnSegments(msg, "live draft that is longer");
    expect(segments).toEqual([
      { kind: "text", text: "final" },
      { kind: "tool", event: tool("a", 1, 5) },
      { kind: "text", text: " text" },
    ]);
  });

  it("clamps anchors beyond the text and keeps consecutive calls adjacent", () => {
    const msg = {
      content: "short",
      tools: [
        tool("x", 1, 999),
        tool("y", 2, 5),
      ],
    };
    const { segments } = buildTurnSegments(msg, "");
    expect(segments).toEqual([
      { kind: "text", text: "short" },
      { kind: "tool", event: tool("x", 1, 999) },
      { kind: "tool", event: tool("y", 2, 5) },
    ]);
  });

  it("stacks unanchored (legacy) calls after the text", () => {
    const msg = { content: "hello", tools: [tool("old", 1)] };
    const { segments, unanchored } = buildTurnSegments(msg, "");
    expect(segments).toEqual([{ kind: "text", text: "hello" }]);
    expect(unanchored).toHaveLength(1);
    expect(unanchored[0].id).toBe("old");
  });
});
