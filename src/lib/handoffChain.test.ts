import { describe, expect, it } from "vitest";
import {
  chainSteps,
  handoffPrompt,
  nextChainStep,
  stepNeedsApproval,
} from "./handoffChain";

describe("handoffChain", () => {
  it("expands each chain into ordered steps", () => {
    expect(chainSteps("none")).toEqual([]);
    expect(chainSteps("plan-act")).toEqual(["plan", "act"]);
    expect(chainSteps("act-review")).toEqual(["act", "review"]);
    expect(chainSteps("plan-act-review")).toEqual(["plan", "act", "review"]);
  });

  it("only plan steps need human approval", () => {
    expect(stepNeedsApproval("plan")).toBe(true);
    expect(stepNeedsApproval("act")).toBe(false);
    expect(stepNeedsApproval("review")).toBe(false);
  });

  it("advances to the next step and stops at the end", () => {
    expect(nextChainStep("plan-act", 0)).toBe("act");
    expect(nextChainStep("plan-act", 1)).toBeNull();
    expect(nextChainStep("act-review", 0)).toBe("review");
    expect(nextChainStep("act-review", 1)).toBeNull();
    expect(nextChainStep("plan-act-review", 0)).toBe("act");
    expect(nextChainStep("plan-act-review", 1)).toBe("review");
    expect(nextChainStep("plan-act-review", 2)).toBeNull();
    expect(nextChainStep("none", 0)).toBeNull();
  });

  it("carries the prior phase result into the next phase prompt", () => {
    expect(handoffPrompt("Fix the bug", null)).toBe("Fix the bug");
    expect(handoffPrompt("Fix the bug", "")).toBe("Fix the bug");
    expect(handoffPrompt("Fix the bug", "  ")).toBe("Fix the bug");
    const out = handoffPrompt("Fix the bug", "Changed src/a.ts");
    expect(out).toContain("Fix the bug");
    expect(out).toContain("## Result from the previous phase");
    expect(out).toContain("Changed src/a.ts");
  });
});
