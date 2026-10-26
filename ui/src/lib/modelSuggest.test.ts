import { describe, expect, it } from "vitest";
import { suggestLlmParams, type SystemSnapshot } from "./modelSuggest";
import type { ModelInfo } from "../context/AppContext";

/** A ModelInfo stub with the fields `suggestLlmParams` reads. */
function model(over: Partial<ModelInfo>): ModelInfo {
  return {
    name: "test",
    architecture: "qwen2",
    nParams: 7e9,
    sizeBytes: 4.4 * 1024 ** 3,
    nCtxTrain: 32768,
    nLayer: 28,
    nEmbd: 3584,
    contextSize: 0,
    ...over,
  } as ModelInfo;
}

/** The 7.8 GiB / 4-core box from the field report, idle. */
const smallBox: SystemSnapshot = {
  cores: 4,
  totalRamGb: 7.8,
  availableRamGb: 6.5,
  gpu: null,
  gpuEnabled: false,
};

/** Same box while the model is already resident: nearly no free RAM. */
const starvedBox: SystemSnapshot = { ...smallBox, availableRamGb: 0.3 };

describe("suggestLlmParams RAM accounting", () => {
  it("reports weights + KV cache, not just the KV cache", () => {
    const weightsGiB = 4.4;
    const roomy: SystemSnapshot = { ...smallBox, totalRamGb: 32, availableRamGb: 28 };
    const s = suggestLlmParams(model({}), roomy);
    expect(s.totalGiB).toBeGreaterThan(weightsGiB);
    expect(s.totalGiB).toBeCloseTo(weightsGiB + s.kvGiB, 5);
    expect(s.fitsRamWithWeights).toBe(true);
  });

  it("flags the weights even when the KV cache alone would fit", () => {
    // 5 GiB free, but the model is 4.4 GiB of weights + its suggested KV cache.
    const tight: SystemSnapshot = { ...smallBox, availableRamGb: 5.0 };
    const s = suggestLlmParams(model({}), tight);
    // The KV cache on its own fits at the suggested context…
    expect(s.fitsRam).toBe(true);
    // …but weights + cache clearly do not.
    expect(s.fitsRamWithWeights).toBe(false);
    expect(s.notes.some((n) => /is free/.test(n))).toBe(true);
  });

  it("reports the deficit when almost no RAM is free", () => {
    const s = suggestLlmParams(model({}), starvedBox);
    expect(s.fitsRamWithWeights).toBe(false);
    expect(s.fitsRam).toBe(false);
    expect(s.notes.some((n) => /only ~0\.3 GiB is free/.test(n))).toBe(true);
  });

  it("flags an F16 build of a small model as wastefully large", () => {
    // 2.7B stored at F16 = ~5 GiB — the real field-report case.
    const f16 = model({ nParams: 2.7e9, sizeBytes: 5.02 * 1024 ** 3 });
    const s = suggestLlmParams(f16, smallBox);
    expect(s.bitsPerWeight).toBeGreaterThan(12);
    expect(s.notes.some((n) => /unquantised \(F16\/BF16\)/.test(n))).toBe(true);
  });

  it("does not warn about a normal Q4 build", () => {
    const q4 = model({ nParams: 2.7e9, sizeBytes: 1.6 * 1024 ** 3 });
    const s = suggestLlmParams(q4, smallBox);
    expect(s.bitsPerWeight).toBeLessThan(6);
    expect(s.notes.some((n) => /unquantised/.test(n))).toBe(false);
  });

  it("treats a missing system snapshot as unknown, not as a failure", () => {
    const s = suggestLlmParams(model({}), null);
    expect(s.fitsRamWithWeights).toBe(true);
    expect(s.fitsRam).toBe(true);
    expect(s.totalGiB).toBeGreaterThan(0);
  });
});
