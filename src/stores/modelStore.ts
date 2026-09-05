import { create } from "zustand";
import type { ModelInfo } from "../types";

interface ModelState {
  /** Currently loaded model (local GGUF or remote). */
  model: ModelInfo | null;
  /** Backend-synced GGUF path (local models only). */
  modelPath: string | null;
  /** Last local GGUF path chosen, used as the picker's default directory. */
  lastPath: string | null;
  /** True while a model load/switch is in flight; gates re-entry. */
  loading: boolean;
  /** 0..1 load progress, null when not loading. */
  progress: number | null;
  /** Recently used model paths (persisted to settings). */
  recentModels: string[];
  /** Set the whole model descriptor. */
  setModel: (m: ModelInfo | null) => void;
  /** Keep the displayed GGUF path in sync with the backend. */
  setModelPath: (p: string | null) => void;
  /** Remember the directory a local model was last chosen from. */
  setLastPath: (p: string | null) => void;
  setLoading: (v: boolean) => void;
  setProgress: (p: number | null) => void;
  setRecentModels: (r: string[]) => void;
  /**
   * Prepend a path to the recent list (dedup, cap at 10) and persist to
   * settings. Accepts the loaded settings object so callers control the write.
   */
  pushRecentModel: (path: string, persist: (next: string[]) => void) => void;
  /** Clear all model state (unload). */
  reset: () => void;
}

export const useModelStore = create<ModelState>((set, get) => ({
  model: null,
  modelPath: null,
  lastPath: null,
  loading: false,
  progress: null,
  recentModels: [],
  setModel: (m) => set({ model: m }),
  setModelPath: (p) => set({ modelPath: p }),
  setLastPath: (p) => set({ lastPath: p }),
  setLoading: (v) => set({ loading: v }),
  setProgress: (p) => set({ progress: p }),
  setRecentModels: (r) => set({ recentModels: r }),
  pushRecentModel: (path, persist) => {
    const next = [path, ...get().recentModels.filter((x) => x !== path)].slice(0, 10);
    set({ recentModels: next });
    if (persist) persist(next);
  },
  reset: () => set({ model: null, modelPath: null, lastPath: null, loading: false, progress: null }),
}));