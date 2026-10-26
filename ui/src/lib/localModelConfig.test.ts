import { describe, expect, it } from "vitest";
import {
  activeLocalConfig,
  applyClearedConfig,
  mergeLocalConfig,
  sanitizeLocalConfig,
  validateLocalModelConfig,
  normalizedConfig,
  DEFAULT_CONFIG_KEY,
} from "./localModelConfig";

describe("activeLocalConfig", () => {
  it("prefers the per-path entry over default", () => {
    const configs = {
      [DEFAULT_CONFIG_KEY]: { contextSize: 4096 },
      "D:\\m.gguf": { contextSize: 8192 },
    };
    expect(activeLocalConfig(configs, "D:\\m.gguf")?.contextSize).toBe(8192);
    expect(activeLocalConfig(configs, "D:\\other.gguf")?.contextSize).toBe(4096);
    expect(activeLocalConfig(configs, null)?.contextSize).toBe(4096);
    expect(activeLocalConfig({}, null)).toBeUndefined();
  });
});

describe("mergeLocalConfig + normalizedConfig", () => {
  it("overlays defined fields only", () => {
    const base = { contextSize: 4096, nThreads: 2 };
    const merged = mergeLocalConfig(base, { contextSize: 8192, nGpuLayers: 24 });
    expect(merged).toEqual({ contextSize: 8192, nThreads: 2, nGpuLayers: 24 });
  });

  it("drops undefined patch fields", () => {
    const merged = mergeLocalConfig({ nGpuLayers: 24 }, { nGpuLayers: undefined, nThreads: 3 });
    expect(merged).toEqual({ nGpuLayers: 24, nThreads: 3 });
  });

  it("normalizedConfig keeps only defined keys", () => {
    expect(normalizedConfig({ contextSize: undefined, nGpuLayers: 0 })).toEqual({ nGpuLayers: 0 });
  });
});

describe("validateLocalModelConfig", () => {
  it("accepts a reasonable config", () => {
    expect(validateLocalModelConfig({ contextSize: 8192, nGpuLayers: 29, nThreads: 4 })).toEqual([]);
  });

  it("rejects out-of-range values", () => {
    const errors = validateLocalModelConfig({ nGpuLayers: -2, nThreads: 0 });
    expect(errors.length).toBeGreaterThanOrEqual(2);
  });

  it("treats backend nulls as unset (no spurious 'got 0' errors)", () => {
    // The Rust backend serializes unset Option fields as null; those must
    // validate as "use the engine default", not as 0.
    const polluted = {
      maxTokens: null,
      nGpuLayers: 0,
    } as unknown as Parameters<typeof validateLocalModelConfig>[0];
    expect(validateLocalModelConfig(polluted)).toEqual([]);
  });

  it("accepts maxTokens -1 (infinite)", () => {
    expect(validateLocalModelConfig({ maxTokens: -1 })).toEqual([]);
  });
});

describe("sanitizeLocalConfig", () => {
  it("drops null, empty-string and NaN values", () => {
    const clean = sanitizeLocalConfig({
      maxTokens: NaN,
      nGpuLayers: 0,
      nThreads: "",
    } as unknown as Record<string, unknown>);
    expect(clean).toEqual({ nGpuLayers: 0 });
  });

  it("merge drops null patch fields instead of persisting them", () => {
    const merged = mergeLocalConfig(
      { contextSize: 4096 },
      { contextSize: null, nThreads: 4 } as unknown as Partial<Parameters<typeof mergeLocalConfig>[1]>,
    );
    expect(merged).toEqual({ contextSize: 4096, nThreads: 4 });
  });

  it("preserves booleans without coercing them to numbers", () => {
    const clean = sanitizeLocalConfig({
      flashAttn: true,
      unifiedKv: false,
      useMmap: true,
      cacheTypeK: "Q8_0",
      llamaArgs: "--swa-full",
    } as unknown as Record<string, unknown>);
    expect(clean).toEqual({
      flashAttn: true,
      unifiedKv: false,
      useMmap: true,
      cacheTypeK: "Q8_0",
      llamaArgs: "--swa-full",
    });
  });

  it("ignores a boolean where a number is expected and vice versa", () => {
    const clean = sanitizeLocalConfig({
      contextSize: true,
      flashAttn: 1,
    } as unknown as Record<string, unknown>);
    expect(clean).toEqual({});
  });
});

describe("applyClearedConfig", () => {
  it("overlays present keys and removes keys explicitly set to null", () => {
    const base = { contextSize: 8192, nBatch: 512, cacheTypeK: "Q8_0", llamaArgs: "--swa-full" };
    const next = applyClearedConfig(base, {
      contextSize: 16384,
      nBatch: null,
      cacheTypeK: "",
      llamaArgs: "--swa-full",
    });
    expect(next).toEqual({ contextSize: 16384, llamaArgs: "--swa-full" });
  });

  it("leaves keys absent from the patch untouched", () => {
    const base = { contextSize: 8192, nThreads: 4 };
    expect(applyClearedConfig(base, { contextSize: 4096 })).toEqual({
      contextSize: 4096,
      nThreads: 4,
    });
  });

  it("sanitizes patched values (invalid patch values are ignored, not written)", () => {
    const base = { contextSize: 8192 };
    // NaN is present in the patch but not a usable number: keep the saved value.
    expect(applyClearedConfig(base, { contextSize: NaN })).toEqual({ contextSize: 8192 });
    // A whitespace-only string clears a string field.
    expect(applyClearedConfig(base, { cacheTypeV: "  " })).toEqual({ contextSize: 8192 });
  });

  it("starts from an empty base without throwing", () => {
    expect(applyClearedConfig(undefined, { flashAttn: false })).toEqual({ flashAttn: false });
  });
});