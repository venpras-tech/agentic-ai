import { beforeEach, describe, expect, it, vi } from "vitest";
import { resetPolicyStoreForTests, usePolicyStore } from "./policyStore";
import type { PolicySnapshot } from "../types";

const snapshot = (): PolicySnapshot => ({
  default: "ask",
  rules: [{ tool: "shell", policy: "allow", commandPatterns: ["*"] }],
});

describe("policyStore", () => {
  beforeEach(() => {
    resetPolicyStoreForTests();
    vi.resetModules();
  });

  it("sets a policy snapshot", () => {
    usePolicyStore.getState().setPolicy(snapshot());
    expect(usePolicyStore.getState().policy?.default).toBe("ask");
  });

  it("clears policy on unload", () => {
    usePolicyStore.getState().setPolicy(snapshot());
    resetPolicyStoreForTests();
    expect(usePolicyStore.getState().policy).toBeNull();
  });
});