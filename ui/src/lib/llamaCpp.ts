// Pure helpers backing the Settings → llama.cpp internals page. Everything here
// is a plain function of its arguments so the panel's estimates and change
// detection can be unit-tested without a running engine.

import type { LlamaLoadConfig } from "./ipc";

/** The subset of model metadata the memory estimate needs (mirrors `ModelInfo`). */
export interface KvModelShape {
  nLayer: number;
  nEmbd: number;
  /** RoPE-free KV head counts, when known, give a closer estimate. */
  nHeadKv?: number;
  nHead?: number;
}

/** Bytes per element for the KV-cache quantization types the app exposes. */
export function bytesPerElement(cacheType: string | null | undefined): number {
  switch ((cacheType || "F16").trim().toUpperCase().replace(".", "_")) {
    case "F32":
      return 4;
    case "F16":
    case "BF16":
      return 2;
    case "Q8_0":
    case "Q8_1":
    case "Q8_K":
      return 1;
    case "Q5_0":
    case "Q5_1":
    case "Q5_K":
      // 5 bits + block scale → ~0.6875 B.
      return 0.6875;
    case "Q4_0":
    case "Q4_1":
    case "Q4_K":
    case "IQ4_NL":
    case "IQ4_XS":
      // 4 bits + block scale → ~0.5625 B.
      return 0.5625;
    case "Q3_K":
    case "IQ3_S":
    case "IQ3_XXS":
      return 0.4375;
    case "Q2_K":
    case "IQ2_S":
    case "IQ2_XS":
    case "IQ2_XXS":
      return 0.3125;
    case "IQ1_S":
    case "IQ1_M":
      return 0.1875;
    default:
      return 2;
  }
}

/**
 * Estimate the KV cache in bytes for a context of `contextSize` tokens.
 *
 * llama.cpp stores 2 tensors (K and V) per layer, each `n_ctx * n_embd_kv`
 * wide. `n_embd_kv` is `n_embd` for models without grouped-query attention; when
 * the model exposes its KV head count it is `nHeadKv * headDim`. This matches
 * the heuristic the backend's model-fit estimator uses, and is deliberately an
 * estimate (the exact figure depends on tensor layout llama.cpp controls).
 */
export function estimateKvBytes(
  model: KvModelShape,
  contextSize: number,
  cacheTypeK?: string | null,
  cacheTypeV?: string | null,
): number {
  const layers = Math.max(0, model.nLayer || 0);
  const ctx = Math.max(0, contextSize || 0);
  const headDim = model.nHead ? model.nEmbd / model.nHead : 0;
  const kvEmbd =
    model.nHeadKv && headDim > 0 ? model.nHeadKv * headDim : model.nEmbd || 0;
  const kBytes = bytesPerElement(cacheTypeK);
  const vBytes = bytesPerElement(cacheTypeV);
  return layers * ctx * kvEmbd * (kBytes + vBytes);
}

/** Round a byte count to a short human string (`224 MiB`, `1.8 GiB`). */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "0 MiB";
  const mib = bytes / (1024 * 1024);
  if (mib < 1024) return `${mib < 10 ? mib.toFixed(1) : Math.round(mib)} MiB`;
  const gib = mib / 1024;
  return `${gib < 10 ? gib.toFixed(2) : gib.toFixed(1)} GiB`;
}

/**
 * Fields the llama.cpp page can edit, mapped to the `ModelInitParams` key each
 * one writes. Kept as data so the change-detection and form rendering agree.
 */
export const LLAMA_EDIT_FIELDS = [
  "contextSize",
  "nBatch",
  "nUbatch",
  "nWorkers",
  "flashAttn",
  "unifiedKv",
  "offloadKqv",
  "useMmap",
  "useMlock",
  "ropeFreqBase",
  "ropeFreqScale",
  "cacheTypeK",
  "cacheTypeV",
  "llamaArgs",
] as const;

export type LlamaEditField = (typeof LLAMA_EDIT_FIELDS)[number];

/** Normalize a raw config so `null`/`undefined`/`""`/`NaN` all read as absent. */
function present(v: unknown): unknown {
  if (v === undefined || v === null) return undefined;
  if (typeof v === "string" && v.trim() === "") return undefined;
  if (typeof v === "number" && Number.isNaN(v)) return undefined;
  return v;
}

/**
 * Names of the editable fields whose value differs between what the engine is
 * running with (`current`) and the draft the user is editing (`draft`). Only
 * fields the draft actually sets are considered, so clearing a field means
 * "fall back to default" rather than "change to undefined".
 */
export function changedFields(
  current: LlamaLoadConfig | null,
  draft: LlamaLoadConfig,
): LlamaEditField[] {
  const out: LlamaEditField[] = [];
  for (const key of LLAMA_EDIT_FIELDS) {
    const next = present((draft as Record<string, unknown>)[key]);
    if (next === undefined) continue;
    const prev = present(
      (current as Record<string, unknown> | null)?.[key],
    );
    if (prev !== next) out.push(key);
  }
  return out;
}

/**
 * True when the cached KV-cache type differs from the newly requested one in a
 * way that requires a fresh (full) context — i.e. reload, not just a
 * regeneration. llama.cpp cannot change cache quantization in place.
 */
export function requiresContextRebuild(
  current: LlamaLoadConfig | null,
  draft: LlamaLoadConfig,
): boolean {
  const rebuildKeys: LlamaEditField[] = [
    "contextSize",
    "cacheTypeK",
    "cacheTypeV",
    "nBatch",
    "nUbatch",
    "flashAttn",
    "unifiedKv",
    "offloadKqv",
    "ropeFreqBase",
    "ropeFreqScale",
  ];
  const changed = new Set(changedFields(current, draft));
  return rebuildKeys.some((k) => changed.has(k));
}
