import { beforeEach, describe, expect, it } from "vitest";
import {
  resetAgentRunStoreForTests,
  runStreaming,
  useAgentRunStore,
} from "./agentRunStore";
import type { InferenceDone } from "../types";

const done = (): InferenceDone => ({
  totalTokens: 10,
  generatedChars: 40,
  tokensPerSec: 5,
  elapsedMs: 1000,
  stopReason: "normal",
  outcome: "completed",
  inputTokens: 20,
  outputTokens: 12,
  cacheReadTokens: 0,
  cacheWriteTokens: 0,
  reasoningTokens: 0,
});

describe("agentRunStore", () => {
  beforeEach(() => {
    resetAgentRunStoreForTests();
  });

  it("tracks the streaming lifecycle", () => {
    const s = useAgentRunStore.getState();
    s.setActiveSessionId(7);
    s.setIsStreaming(true);
    expect(useAgentRunStore.getState().activeSessionId).toBe(7);
    expect(runStreaming()).toBe(true);
    s.setIsStreaming(false);
    expect(runStreaming()).toBe(false);
  });

  it("records lastDone and resets run-scoped fields together", () => {
    const s = useAgentRunStore.getState();
    s.setActiveSessionId(7);
    s.setIsStreaming(true);
    s.setCurrentStep(2);
    s.setCurrentSubtask({ index: 0, total: 3, title: "impl" });
    s.setLastDone(done());
    s.resetRun();
    const r = useAgentRunStore.getState();
    expect(r.activeSessionId).toBeNull();
    expect(r.isStreaming).toBe(false);
    expect(r.currentStep).toBeNull();
    expect(r.currentSubtask).toBeNull();
    expect(r.lastDone).toBeTruthy();
  });

  it("tracks step + subtask independently", () => {
    const s = useAgentRunStore.getState();
    s.setCurrentStep(4);
    s.setCurrentSubtask({ index: 1, total: 2, title: "verify" });
    const r = useAgentRunStore.getState();
    expect(r.currentStep).toBe(4);
    expect(r.currentSubtask?.title).toBe("verify");
  });

  it("upserts running sub-tasks keyed by (index, total) and preserves start time", () => {
    const s = useAgentRunStore.getState();
    s.upsertSubtask({
      index: 1,
      total: 2,
      title: "a",
      model: "qwen",
      tool: "read_file_range",
      startedAt: 1000,
    });
    s.upsertSubtask({
      index: 2,
      total: 2,
      title: "b",
      model: "qwen",
      startedAt: 2000,
    });
    expect(useAgentRunStore.getState().runningSubtasks).toHaveLength(2);

    // A refresh event for subtask 1 (tool change) keeps its original start.
    s.upsertSubtask({ index: 1, total: 2, title: "a", model: "qwen", tool: "run_tests" });
    const rows = useAgentRunStore.getState().runningSubtasks;
    const a = rows.find((r) => r.index === 1);
    expect(a?.tool).toBe("run_tests");
    expect(a?.startedAt).toBe(1000);
    expect(rows).toHaveLength(2);
  });

  it("removes a finished sub-task and drops the whole list on reset", () => {
    const s = useAgentRunStore.getState();
    s.upsertSubtask({ index: 1, total: 2, title: "a", startedAt: 1000 });
    s.removeSubtask(1, 2);
    expect(useAgentRunStore.getState().runningSubtasks).toHaveLength(0);

    s.upsertSubtask({ index: 2, total: 2, title: "b", startedAt: 2000 });
    s.setCurrentStep(1);
    s.resetRun();
    const r = useAgentRunStore.getState();
    expect(r.runningSubtasks).toHaveLength(0);
    expect(r.currentStep).toBeNull();
  });

  it("queues prompts while busy and drains them FIFO after reset", () => {
    const s = useAgentRunStore.getState();
    expect(s.shiftPrompt()).toBeUndefined();
    s.enqueuePrompt({ text: "one" });
    s.enqueuePrompt({ text: "two", opts: { verify: true } });
    expect(useAgentRunStore.getState().queuedCount).toBe(2);
    // resetRun (turn boundary) must NOT drop queued messages.
    s.resetRun();
    expect(useAgentRunStore.getState().queuedCount).toBe(2);
    expect(s.shiftPrompt()?.text).toBe("one");
    const last = s.shiftPrompt();
    expect(last?.text).toBe("two");
    expect(last?.opts?.verify).toBe(true);
    expect(useAgentRunStore.getState().queuedCount).toBe(0);
  });
});