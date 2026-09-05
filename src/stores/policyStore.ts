import { create } from "zustand";
import type { PolicySnapshot } from "../types";
import { api } from "../lib/ipc";

interface PolicyState {
  /** Latest policy snapshot from the backend (tool allow/deny state). */
  policy: PolicySnapshot | null;
  setPolicy: (p: PolicySnapshot | null) => void;
  /** Refresh the snapshot from the backend. */
  refreshPolicy: () => void;
}

export const usePolicyStore = create<PolicyState>((set) => ({
  policy: null,
  setPolicy: (p) => set({ policy: p }),
  refreshPolicy: () => {
    api
      .agentPolicySnapshot()
      .then((p) => set({ policy: p }))
      .catch(() => {});
  },
}));

export function resetPolicyStoreForTests() {
  usePolicyStore.setState({ policy: null });
}