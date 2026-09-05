import { create } from "zustand";

import { api, isTauriRuntime } from "../lib/ipc";
import { AGENT_SYSTEM_PROMPT } from "../lib/prompt";
import type { AgentMode } from "../types";

interface CustomModesState {
  /** User-defined agent modes for the current workspace (`.ai/modes/*.md`). */
  customModes: AgentMode[];
  activeCustomMode: string | null;
  /** Latest-committed active mode object so async handlers read a stable ref. */
  activeModeRef: AgentMode | null;
  /** Full system prompt with the active custom mode's instructions appended. */
  modeAwareSystemPrompt: () => string;
  /** Reload custom modes whenever the workspace changes. */
  syncWithWorkspace: (workspaceRoot: string | null) => Promise<void>;
  /** Apply a custom mode: reset the application ID, system prompt, and ref. */
  applyCustomMode: (name: string | null) => Promise<void>;
}

const buildPrompt = (mode: AgentMode | null): string =>
  mode
    ? `${AGENT_SYSTEM_PROMPT}

## Active mode: ${mode.name}
${mode.systemPrompt}`
    : AGENT_SYSTEM_PROMPT;

/**
 * Zustand slice owning the workspace's custom agent modes (`.ai/modes/*.md`).
 * Extracted from App.tsx so mode lifecycle (load / reload / apply / system
 * prompt) lives in one place instead of 20 lines of inline state + ref + effect
 * in the root component. The store keeps its own `activeModeRef` so every
 * async handler sees the latest committed mode without stale closures.
 */
export const useCustomModes = create<CustomModesState>((set, get) => ({
  customModes: [],
  activeCustomMode: null,
  activeModeRef: null,

  modeAwareSystemPrompt: () => buildPrompt(get().activeModeRef),

  syncWithWorkspace: async (workspaceRoot) => {
    if (!isTauriRuntime() || !workspaceRoot) {
      set({ customModes: [], activeCustomMode: null, activeModeRef: null });
      return;
    }
    try {
      const modes = await api.modesLoad();
      set((s) => {
        const active = s.activeCustomMode;
        if (active && !modes.some((m) => m.name === active)) {
          return {
            customModes: modes,
            activeCustomMode: null,
            activeModeRef: null,
          };
        }
        return {
          customModes: modes,
          activeModeRef:
            modes.find((m) => m.name === active) ?? s.activeModeRef,
        };
      });
    } catch {
      set({ customModes: [] });
    }
  },

  applyCustomMode: async (name) => {
    const { customModes } = get();
    const mode = customModes.find((m) => m.name === name) ?? null;
    set({ activeCustomMode: mode?.name ?? null, activeModeRef: mode });
    if (isTauriRuntime()) {
      try {
        await api.contextSetSystemPrompt(buildPrompt(mode));
      } catch {
        /* no active context yet — prompt applies on next model load */
      }
    }
  },
}));