// Pure "mode handoff" chain model (Plan -> Act -> Review), mirroring the
// industry pattern of chaining agentic phases within one task. Kept dependency-
// free and deterministic so the transition logic is unit-testable.

export type HandoffStep = "plan" | "act" | "review";

export type HandoffChain = "none" | "plan-act" | "act-review" | "plan-act-review";

export const HANDOFF_OPTIONS: {
  value: HandoffChain;
  label: string;
  title: string;
}[] = [
  { value: "none", label: "Off", title: "Run the selected phase once" },
  {
    value: "plan-act",
    label: "Plan → Act",
    title: "Draft a plan, then (after you approve) execute it",
  },
  {
    value: "act-review",
    label: "Act → Review",
    title: "Execute, then automatically review the changes",
  },
  {
    value: "plan-act-review",
    label: "Plan → Act → Review",
    title: "Draft a plan, execute after approval, then auto-review",
  },
];

export function chainSteps(chain: HandoffChain): HandoffStep[] {
  switch (chain) {
    case "plan-act":
      return ["plan", "act"];
    case "act-review":
      return ["act", "review"];
    case "plan-act-review":
      return ["plan", "act", "review"];
    default:
      return [];
  }
}

/** The `SendMode`-style flags a step maps to, for building a `SendOptions`. */
export function stepSendMode(step: HandoffStep): "plan" | "act" | "review" {
  return step;
}

/** A plan step is the only one that pauses for human approval. */
export function stepNeedsApproval(step: HandoffStep): boolean {
  return step === "plan";
}

/**
 * Given the current step index within a chain, return the next step or `null`
 * when the chain is exhausted. Callers pass this into the auto-continuation.
 */
export function nextChainStep(
  chain: HandoffChain,
  currentIndex: number,
): HandoffStep | null {
  const steps = chainSteps(chain);
  if (currentIndex < 0 || currentIndex >= steps.length - 1) return null;
  return steps[currentIndex + 1];
}

/** Build the prompt for a handoff phase, carrying the prior phase's result. */
export function handoffPrompt(basePrompt: string, result: string | null): string {
  if (!result || !result.trim()) return basePrompt;
  return `${basePrompt}\n\n## Result from the previous phase\n\n${result.trim()}`;
}
