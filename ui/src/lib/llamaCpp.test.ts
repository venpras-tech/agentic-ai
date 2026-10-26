import { describe, it, expect } from "vitest";
import {
  bytesPerElement,
  changedFields,
  estimateKvBytes,
  formatBytes,
  requiresContextRebuild,
} from "./llamaCpp";
import type { LlamaLoadConfig } from "./ipc";

// Qwen2.5-Coder-7B-ish shape: 28 layers, 3584 embd, GQA with 4 KV heads of 128.
const qwen7b = { nLayer: 28, nEmbd: 3584, nHeadKv: 4, nHead: 28 };

describe("bytesPerElement", () => {
  it("maps the full-precision and 8-bit types", () => {
    expect(bytesPerElement("F32")).toBe(4);
    expect(bytesPerElement("F16")).toBe(2);
    expect(bytesPerElement("Q8_0")).toBe(1);
  });

  it("maps the block-quantized types and is case/format tolerant", () => {
    expect(bytesPerElement("q4_0")).toBeCloseTo(0.5625);
    expect(bytesPerElement("Q5_K")).toBeCloseTo(0.6875);
  });

  it("treats an absent type as F16 (the llama.cpp default)", () => {
    expect(bytesPerElement(null)).toBe(2);
    expect(bytesPerElement(undefined)).toBe(2);
    expect(bytesPerElement("")).toBe(2);
  });
});

describe("estimateKvBytes", () => {
  it("uses the KV head count when the model exposes grouped-query attention", () => {
    // 28 layers * 4096 ctx * (4 * 128) kv-embd * (2 + 2) bytes.
    const bytes = estimateKvBytes(qwen7b, 4096);
    expect(bytes).toBe(28 * 4096 * 512 * 4);
    expect(formatBytes(bytes)).toBe("224 MiB");
  });

  it("falls back to the full embedding width without a KV head count", () => {
    const bytes = estimateKvBytes({ nLayer: 12, nEmbd: 768 }, 2048);
    expect(bytes).toBe(12 * 2048 * 768 * 4);
  });

  it("scales linearly with context and halves at Q8_0", () => {
    const f16 = estimateKvBytes(qwen7b, 8192);
    expect(estimateKvBytes(qwen7b, 16384)).toBe(f16 * 2);
    const q8 = estimateKvBytes(qwen7b, 8192, "Q8_0", "Q8_0");
    expect(q8).toBe(f16 / 2);
  });

  it("is zero for an unloaded / empty model", () => {
    expect(estimateKvBytes({ nLayer: 0, nEmbd: 0 }, 4096)).toBe(0);
    expect(estimateKvBytes(qwen7b, 0)).toBe(0);
  });
});

describe("formatBytes", () => {
  it("rounds MiB and GiB to readable strings", () => {
    expect(formatBytes(0)).toBe("0 MiB");
    expect(formatBytes(224 * 1024 * 1024)).toBe("224 MiB");
    expect(formatBytes(2 * 1024 * 1024 * 1024)).toBe("2.00 GiB");
    expect(formatBytes(-5)).toBe("0 MiB");
  });
});

describe("changedFields", () => {
  it("reports only fields the draft sets to a different value", () => {
    const current: LlamaLoadConfig = { contextSize: 8192, nBatch: 512 };
    expect(changedFields(current, { contextSize: 16384 })).toEqual(["contextSize"]);
    expect(changedFields(current, { contextSize: 8192, nBatch: 512 })).toEqual([]);
  });

  it("treats clearing a field as 'leave it at default', not a change", () => {
    const current: LlamaLoadConfig = { contextSize: 8192 };
    expect(changedFields(current, { contextSize: null })).toEqual([]);
    expect(changedFields(current, { contextSize: undefined })).toEqual([]);
  });

  it("detects a boolean flip", () => {
    expect(changedFields({ flashAttn: false }, { flashAttn: true })).toEqual(["flashAttn"]);
  });
});

describe("requiresContextRebuild", () => {
  it("is true when the context size changes (needs a full reload)", () => {
    expect(requiresContextRebuild({ contextSize: 8192 }, { contextSize: 16384 })).toBe(true);
    expect(requiresContextRebuild({}, { cacheTypeK: "Q8_0" })).toBe(true);
  });

  it("is false for a pure sampling/thread change", () => {
    expect(requiresContextRebuild({ nThreads: 4 }, { nThreads: 8 })).toBe(false);
    expect(requiresContextRebuild({}, {})).toBe(false);
  });
});
