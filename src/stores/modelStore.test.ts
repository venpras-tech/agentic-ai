import { beforeEach, describe, expect, it, vi } from "vitest";
import { useModelStore } from "./modelStore";
import type { ModelInfo } from "../types";

describe("modelStore", () => {
  beforeEach(() => {
    useModelStore.setState({
      model: null,
      modelPath: null,
      lastPath: null,
      loading: false,
      progress: null,
      recentModels: [],
    });
  });

  it("tracks the loaded model descriptor", () => {
    const info = { name: "qwen2.5:7b", path: "/m/q.gguf" } as unknown as ModelInfo;
    useModelStore.getState().setModel(info);
    expect(useModelStore.getState().model).toEqual(info);
  });

  it("tracks backend-synced path and remembered directory independently", () => {
    useModelStore.getState().setModelPath("/m/a.gguf");
    useModelStore.getState().setLastPath("/m");
    expect(useModelStore.getState().modelPath).toBe("/m/a.gguf");
    expect(useModelStore.getState().lastPath).toBe("/m");
  });

  it("gates re-entry with loading + exposes progress", () => {
    useModelStore.getState().setLoading(true);
    useModelStore.getState().setProgress(0.5);
    expect(useModelStore.getState().loading).toBe(true);
    expect(useModelStore.getState().progress).toBe(0.5);
  });

  it("prepends path to recentModels, dedups, caps at 10 and persists", () => {
    const persist = vi.fn();
    const push = useModelStore.getState().pushRecentModel;
    for (let i = 1; i <= 11; i++) push(`/m/m${i}.gguf`, () => {});
    expect(useModelStore.getState().recentModels).toHaveLength(10);
    expect(useModelStore.getState().recentModels[0]).toBe("/m/m11.gguf");
    push("/m/m5.gguf", persist);
    const list = useModelStore.getState().recentModels;
    expect(list[0]).toBe("/m/m5.gguf");
    expect(list.filter((x) => x === "/m/m5.gguf")).toHaveLength(1);
    expect(persist).toHaveBeenCalledWith(list);
  });

  it("resets all state on unload", () => {
    const s = useModelStore.getState();
    s.setModel({ name: "x" } as unknown as ModelInfo);
    s.setModelPath("/m/x.gguf");
    s.setLastPath("/m");
    s.setLoading(true);
    s.setProgress(0.9);
    s.reset();
    const r = useModelStore.getState();
    expect(r.model).toBeNull();
    expect(r.modelPath).toBeNull();
    expect(r.lastPath).toBeNull();
    expect(r.loading).toBe(false);
    expect(r.progress).toBeNull();
  });
});