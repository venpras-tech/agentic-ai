import { describe, expect, it } from "vitest";
import { parseChatSegments, stripChatTags } from "./chatParse";

describe("parseChatSegments", () => {
  it("returns plain text as a single text segment", () => {
    const segments = parseChatSegments("Hello world");
    expect(segments).toEqual([{ kind: "text", text: "Hello world" }]);
  });

  it("splits reasoning and text", () => {
    const segments = parseChatSegments(
      "Let me think.\n\n<agent_thought>Deep dive</agent_thought>\n\nThen I acted.",
    );
    expect(segments).toEqual([
      { kind: "text", text: "Let me think.\n\n" },
      { kind: "reasoning", text: "Deep dive" },
      { kind: "text", text: "\n\nThen I acted." },
    ]);
  });

  it("parses a tool_call with name/status attrs", () => {
    const segments = parseChatSegments(
      '<tool_call name="run_command" status="success">\n{"cmd":"ls"}\n</tool_call>',
    );
    expect(segments).toHaveLength(1);
    expect(segments[0]).toMatchObject({
      kind: "tool",
      name: "run_command",
      status: "success",
    });
    expect((segments[0] as { text: string }).text).toContain('"ls"');
  });

  it("defaults an empty tool_call to running", () => {
    const segments = parseChatSegments("<tool_call name=\"grep\"></tool_call>");
    expect(segments[0]).toMatchObject({ kind: "tool", name: "grep", status: "running" });
  });

  it("parses a diff with a path attr", () => {
    const segments = parseChatSegments(
      '<diff path="src/a.ts">\n--- a/src/a.ts\n+++ b/src/a.ts\n</diff>',
    );
    expect(segments).toHaveLength(1);
    expect(segments[0]).toMatchObject({ kind: "diff", path: "src/a.ts" });
  });

  it("reads a hitl_gate reason from the attribute", () => {
    const segments = parseChatSegments(
      '<hitl_gate reason="about to commit">Commit approval</hitl_gate>',
    );
    expect(segments[0]).toMatchObject({ kind: "gate", reason: "about to commit" });
  });

  it("reads a hitl_gate reason from the body when no attr", () => {
    const segments = parseChatSegments("<hitl_gate>Deleting a folder</hitl_gate>");
    expect(segments[0]).toMatchObject({ kind: "gate", reason: "Deleting a folder" });
  });

  it("tolerates an unclosed trailing tag (streaming)", () => {
    const segments = parseChatSegments(
      "Before.\n\n<agent_thought>Still reasoning",
    );
    expect(segments[segments.length - 1]).toEqual({
      kind: "reasoning",
      text: "Still reasoning",
    });
  });

  it("preserves segment order across mixed kinds", () => {
    const segments = parseChatSegments(
      "a\n<agent_thought>r</agent_thought>\nb\n<tool_call>out</tool_call>\nc",
    );
    expect(segments.map((s) => s.kind)).toEqual([
      "text",
      "reasoning",
      "text",
      "tool",
      "text",
    ]);
  });

  it("returns the cached array for repeated identical input", () => {
    const src = "A\n<agent_thought>r</agent_thought>\nB";
    const first = parseChatSegments(src);
    const second = parseChatSegments(src);
    expect(second).toBe(first);
    expect(second).toEqual([
      { kind: "text", text: "A\n" },
      { kind: "reasoning", text: "r" },
      { kind: "text", text: "\nB" },
    ]);
  });

  it("distinguishes distinct inputs", () => {
    const a = parseChatSegments("alpha");
    const b = parseChatSegments("beta");
    expect(a).not.toBe(b);
    expect(a[0]).toEqual({ kind: "text", text: "alpha" });
    expect(b[0]).toEqual({ kind: "text", text: "beta" });
  });

  it("keeps parsing correct past the cache limit", () => {
    for (let i = 0; i < 300; i++) parseChatSegments(`unique-${i}`);
    const segments = parseChatSegments("<tool_call name=\"grep\"></tool_call>");
    expect(segments[0]).toMatchObject({ kind: "tool", name: "grep", status: "running" });
    const plain = parseChatSegments("re-check");
    expect(plain).toEqual([{ kind: "text", text: "re-check" }]);
  });
});

describe("stripChatTags", () => {
  it("removes custom tags and collapses blank runs", () => {
    const out = stripChatTags(
      "Intro\n\n<agent_thought>hidden</agent_thought>\n\n\n\n<tool_call>x</tool_call>\nOutro",
    );
    expect(out).toBe("Intro\n\nOutro");
  });

  it("removes an unclosed trailing tag", () => {
    expect(stripChatTags("Text\n\n<diff path=\"a\">pending")).toBe("Text");
  });
});