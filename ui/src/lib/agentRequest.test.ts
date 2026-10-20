import { describe, it, expect } from "vitest";
import {
  buildAgentRequest,
  buildPlanExecutionRequest,
  num,
  DEFAULT_MAX_TOKENS,
  DEFAULT_MAX_STEPS,
} from "./agentRequest";

const base = {
  prompt: "hello",
  uiMode: "agent",
  settings: {} as Record<string, unknown>,
  maxSteps: DEFAULT_MAX_STEPS,
  verify: true,
  smallModel: false,
};

describe("num", () => {
  it("clamps into range and rounds", () => {
    expect(num(5.6, 1, 1, 10)).toBe(6);
    expect(num(0, 1, 1, 10)).toBe(1);
    expect(num(999, 1, 1, 10)).toBe(10);
  });

  it("falls back on non-numeric input", () => {
    expect(num(undefined, 42, 1, 10)).toBe(42);
    expect(num("nope", 42, 1, 10)).toBe(42);
  });

  it("parses numeric strings", () => {
    expect(num("7", 1, 1, 10)).toBe(7);
  });
});

describe("buildAgentRequest", () => {
  it("reads max tokens and steps from settings", () => {
    const req = buildAgentRequest({
      ...base,
      settings: { params: { maxTokens: 2048, maxSteps: 7 } },
    });
    expect(req.maxTokens).toBe(2048);
    expect(req.maxSteps).toBe(7);
  });

  it("accepts snake_case settings keys", () => {
    const req = buildAgentRequest({
      ...base,
      settings: { params: { max_tokens: 512, max_steps: 3 } },
    });
    expect(req.maxTokens).toBe(512);
    expect(req.maxSteps).toBe(3);
  });

  it("falls back to defaults when unset", () => {
    const req = buildAgentRequest(base);
    expect(req.maxTokens).toBe(DEFAULT_MAX_TOKENS);
    expect(req.maxSteps).toBe(DEFAULT_MAX_STEPS);
  });

  it("caps steps for very small local models", () => {
    const req = buildAgentRequest({
      ...base,
      smallModel: true,
      settings: { params: { maxSteps: 20 } },
    });
    expect(req.maxSteps).toBe(4);
  });

  it("enables plan mode for the plan UI mode", () => {
    expect(buildAgentRequest({ ...base, uiMode: "plan" }).planMode).toBe(true);
    expect(buildAgentRequest({ ...base, uiMode: "agent" }).planMode).toBe(false);
  });

  it("never verifies in ask mode", () => {
    expect(buildAgentRequest({ ...base, uiMode: "ask", verify: true }).verify).toBe(false);
    expect(buildAgentRequest({ ...base, uiMode: "agent", verify: true }).verify).toBe(true);
  });

  it("omits a zero or missing token budget", () => {
    expect(buildAgentRequest(base).tokenBudget).toBeUndefined();
    const zero = buildAgentRequest({ ...base, settings: { tokenBudget: 0 } });
    expect(zero.tokenBudget).toBeUndefined();
  });

  it("forwards a token budget from either nesting", () => {
    expect(
      buildAgentRequest({ ...base, settings: { params: { tokenBudget: 900 } } }).tokenBudget,
    ).toBe(900);
    expect(buildAgentRequest({ ...base, settings: { token_budget: 700 } }).tokenBudget).toBe(700);
  });

  it("prefers an explicit token budget over settings", () => {
    const req = buildAgentRequest({ ...base, tokenBudget: 250, settings: { params: { tokenBudget: 900 } } });
    expect(req.tokenBudget).toBe(250);
    // An explicit 0 (Off) also wins over a budget buried in settings.
    expect(buildAgentRequest({ ...base, tokenBudget: 0, settings: { tokenBudget: 900 } }).tokenBudget).toBeUndefined();
  });

  it("prefers an explicit decompose flag over settings", () => {
    expect(buildAgentRequest({ ...base, decompose: true }).decompose).toBe(true);
    expect(buildAgentRequest({ ...base, decompose: false, settings: { params: { decompose: true } } }).decompose).toBe(false);
    expect(buildAgentRequest({ ...base, settings: { params: { decompose: true } } }).decompose).toBe(true);
    expect(buildAgentRequest(base).decompose).toBe(false);
  });

  it("omits images when none are attached", () => {
    expect(buildAgentRequest(base).images).toBeUndefined();
  });

  it("attaches images when present", () => {
    const images = [{ dataUrl: "data:image/png;base64,AAA" }];
    expect(buildAgentRequest({ ...base, images }).images).toEqual(images);
  });
});

describe("buildPlanExecutionRequest", () => {
  it("clears plan mode so the agent may edit", () => {
    const plan = buildAgentRequest({ ...base, uiMode: "plan" });
    expect(plan.planMode).toBe(true);
    const exec = buildPlanExecutionRequest(plan);
    expect(exec.planMode).toBe(false);
  });

  it("does not mutate the original plan request", () => {
    const plan = buildAgentRequest({ ...base, uiMode: "plan" });
    buildPlanExecutionRequest(plan);
    expect(plan.planMode).toBe(true);
  });
});
