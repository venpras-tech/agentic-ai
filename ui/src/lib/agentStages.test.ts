import { describe, expect, it } from "vitest";
import {
  IDLE_STAGE,
  isStageVisible,
  reduceAgentStage,
  stageOrQueued,
} from "./agentStages";

const run = (events: { type: string; [k: string]: unknown }[]) =>
  events.reduce(
    (prev, e) =>
      reduceAgentStage(
        prev,
        e as Parameters<typeof reduceAgentStage>[1],
        1000 + prev.steps,
      ),
    IDLE_STAGE,
  );

describe("reduceAgentStage", () => {
  it("starts idle and stays hidden", () => {
    expect(IDLE_STAGE.stage).toBe("idle");
    expect(isStageVisible(IDLE_STAGE)).toBe(false);
  });

  it("goes queued -> thinking -> responding as the reply streams", () => {
    const s = run([{ type: "queued" }, { type: "started" }, { type: "token" }]);
    expect(s.stage).toBe("responding");
    expect(s.label).toBe("Responding…");
    expect(isStageVisible(s)).toBe(true);
  });

  it("does not restart the timer while tokens keep arriving", () => {
    const first = reduceAgentStage(IDLE_STAGE, { type: "started" }, 5);
    const a = reduceAgentStage(first, { type: "token" }, 10);
    const b = reduceAgentStage(a, { type: "token" }, 99);
    expect(b).toBe(a);
    expect(b.since).toBe(10);
  });

  it("names the running tool, then falls back to Working", () => {
    const started = reduceAgentStage(IDLE_STAGE, { type: "started" }, 0);
    const running = reduceAgentStage(started, { type: "tool", tool: "write_file", status: "running" }, 1);
    expect(running.stage).toBe("working");
    expect(running.label).toBe("Running write_file…");
    const done = reduceAgentStage(running, { type: "tool", tool: "write_file", status: "done" }, 2);
    expect(done.label).toBe("Working…");
    expect(done.detail).toBeUndefined();
  });

  it("counts steps without clobbering a streaming reply", () => {
    const responding = run([{ type: "started" }, { type: "token" }]);
    const stepped = reduceAgentStage(responding, { type: "step" }, 7);
    expect(stepped.steps).toBe(1);
    expect(stepped.stage).toBe("responding");
    expect(stepped.label).toBe("Responding…");
  });

  it("resets the step counter on a new run", () => {
    const a = run([{ type: "started" }, { type: "step" }, { type: "step" }]);
    expect(a.steps).toBe(2);
    const b = reduceAgentStage(a, { type: "started" }, 3);
    expect(b.steps).toBe(0);
  });

  it("hides once the run finishes", () => {
    const s = run([{ type: "started" }, { type: "done" }]);
    expect(s.stage).toBe("done");
    expect(isStageVisible(s)).toBe(false);
  });

  it("ignores tool/step noise after the run ended", () => {
    const done = run([{ type: "started" }, { type: "done" }]);
    expect(reduceAgentStage(done, { type: "tool", tool: "x", status: "running" }, 1)).toBe(done);
    expect(reduceAgentStage(done, { type: "step" }, 1)).toBe(done);
    expect(reduceAgentStage(done, { type: "prompt", tokens: 512 }, 1)).toBe(done);
  });

  it("records prompt size during thinking without resetting the timer", () => {
    const started = reduceAgentStage(IDLE_STAGE, { type: "started" }, 5);
    expect(started.since).toBe(5);
    const withPrompt = reduceAgentStage(started, { type: "prompt", tokens: 2048 }, 9);
    expect(withPrompt.stage).toBe("thinking");
    expect(withPrompt.label).toBe("Thinking…");
    expect(withPrompt.promptTokens).toBe(2048);
    // The elapsed timer must still start at `started`, not when the prompt-size
    // detail arrived a moment later.
    expect(withPrompt.since).toBe(5);
  });

  it("drops the prompt size once tokens start streaming", () => {
    const thinking = run([{ type: "started" }, { type: "prompt", tokens: 2048 }]);
    expect(thinking.promptTokens).toBe(2048);
    const responding = reduceAgentStage(thinking, { type: "token" }, 11);
    expect(responding.stage).toBe("responding");
    expect(responding.promptTokens).toBeUndefined();
  });

  it("ignores a prompt event when the backend cannot report a size", () => {
    const started = reduceAgentStage(IDLE_STAGE, { type: "started" }, 5);
    const zero = reduceAgentStage(started, { type: "prompt", tokens: 0 }, 6);
    expect(zero.promptTokens).toBeUndefined();
  });
});

describe("stageOrQueued", () => {
  it("shows Sending request… between submit and the first event", () => {
    const s = stageOrQueued(IDLE_STAGE, true);
    expect(s.stage).toBe("queued");
    expect(isStageVisible(s)).toBe(true);
  });

  it("never overrides a real stage", () => {
    const real = run([{ type: "started" }, { type: "token" }]);
    expect(stageOrQueued(real, true)).toBe(real);
  });

  it("stays idle when nothing is running", () => {
    expect(stageOrQueued(IDLE_STAGE, false)).toBe(IDLE_STAGE);
  });
});