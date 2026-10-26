import { describe, expect, it } from "vitest";
import { AGENT_SYSTEM_PROMPT } from "./agentPrompt";
import { COMPACT_AGENT_SYSTEM_PROMPT, buildSystemPrompt } from "./agentPrepare";

describe("agent system prompt", () => {
  it("lets the model answer questions in plain text", () => {
    expect(AGENT_SYSTEM_PROMPT).toMatch(/respond in plain text/i);
    expect(AGENT_SYSTEM_PROMPT).toMatch(/Do NOT call tools/i);
    expect(AGENT_SYSTEM_PROMPT).toMatch(/plain text answer is a complete, valid final response/i);
  });

  it("does not force execution for every turn", () => {
    expect(AGENT_SYSTEM_PROMPT).not.toMatch(/Immediately output tool calls/i);
    expect(AGENT_SYSTEM_PROMPT).not.toMatch(/not a conversational assistant/i);
    expect(AGENT_SYSTEM_PROMPT).not.toMatch(/MUST use your tools/i);
    expect(AGENT_SYSTEM_PROMPT).not.toMatch(/Do not output text outside/i);
  });

  it("still documents the tool call format", () => {
    expect(AGENT_SYSTEM_PROMPT).toContain("<execute_tool>");
    expect(AGENT_SYSTEM_PROMPT).toContain('"tool_name"');
    expect(AGENT_SYSTEM_PROMPT).toContain("read_file_range");
  });

  it("keeps Ask mode read-only while allowing direct answers", () => {
    const prompt = buildSystemPrompt("ask", 7_000_000_000);
    expect(prompt).toMatch(/READ-ONLY/);
    expect(prompt).toMatch(/answer directly in plain text/i);
  });

  it("uses a conversational compact prompt for small models", () => {
    expect(COMPACT_AGENT_SYSTEM_PROMPT).toMatch(/Answer questions in plain text/i);
    expect(COMPACT_AGENT_SYSTEM_PROMPT).not.toMatch(/Implement what they ask using real tools only/i);
  });
});
