import type { LlmSettings, ModelInfo } from "../context/AppContext";

export interface SystemSnapshot {
  cores: number;
  totalRamGb: number;
  availableRamGb: number;
  gpu: { name: string; vramGb: number } | null;
  gpuEnabled: boolean;
}

export interface ModelSuggestions {
  params: Partial<LlmSettings>;
  kvGiB: number;
  fitsRam: boolean;
  bitsPerWeight: number | null;
  quantLevel: "good" | "caution" | "heavy";
  notes: string[];
}

const KV_BYTES_PER_TOKEN = 2 * 2; // K+V, fp16 each
const RESERVE_GiB = 1.0;

export function suggestLlmParams(meta: ModelInfo, sys: SystemSnapshot | null): ModelSuggestions {
  const notes: string[] = [];
  const params: Partial<LlmSettings> = {};

  // 1. Context window: never exceed the model's training context — running a
  //    model past n_ctx_train is the single biggest cause of hallucination and
  //    incoherence. Cap by available RAM too (KV cache grows ~2*n_layer*n_embd
  //    bytes per token).
  const kvPerToken = meta.nLayer * meta.nEmbd * KV_BYTES_PER_TOKEN;
  let context = meta.nCtxTrain > 0 ? meta.nCtxTrain : 4096;
  let kvGiB = (kvPerToken * context) / (1024 * 1024 * 1024);
  if (sys && sys.totalRamGb > 0) {
    const budget = Math.max(0, sys.totalRamGb - RESERVE_GiB);
    if (kvGiB > budget * 0.7) {
      const capped = Math.max(1024, Math.floor((budget * 0.7 * (1024 * 1024 * 1024)) / kvPerToken));
      context = Math.min(context, capped);
      kvGiB = (kvPerToken * context) / (1024 * 1024 * 1024);
      notes.push(`Context capped to ${context.toLocaleString()} tokens so the KV cache (~${kvGiB.toFixed(1)} GiB) fits in RAM.`);
    }
  }
  params.contextSize = context;
  const fitsRam = !sys || sys.availableRamGb >= kvGiB;

  // 2. GPU offload: put every layer on the GPU when one is enabled.
  if (sys?.gpuEnabled && sys.gpu && sys.gpu.vramGb > 0) {
    params.nGpuLayers = meta.nLayer > 0 ? meta.nLayer : 0;
  } else {
    params.nGpuLayers = 0;
  }

  // 3. CPU threads: match physical cores.
  if (sys?.cores && sys.cores > 0) {
    params.nThreads = sys.cores;
  }

  // 4. Generation sampling is deliberately not configurable. The engine uses
  //    llama.cpp's own default sampler chain, so no sampler fields are
  //    suggested or applied.

  // 5. A model below ~3B params generally cannot follow agent-style prompts
  //    (system + tools + memory). Warning beats parameter tuning here.
  if (meta.nParams > 0 && meta.nParams < 3e9) {
    notes.push(
      `This model is only ~${(meta.nParams / 1e9).toFixed(1)}B params — too small to reliably follow agent prompts (tools, memory, instructions). Expect looping and hallucinated output no matter the settings; use at least a 7B instruct model (e.g. Llama-3.1-8B, Qwen2.5-7B) for coding work.`,
    );
  }

  // 6. Quantisation quality: bits-per-weight flags heavily-lossy downloads.
  let bitsPerWeight: number | null = null;
  let quantLevel: ModelSuggestions["quantLevel"] = "good";
  if (meta.nParams > 0 && meta.sizeBytes > 0) {
    bitsPerWeight = (meta.sizeBytes * 8) / meta.nParams;
    if (bitsPerWeight < 3.0) {
      quantLevel = "heavy";
      notes.push(`~${bitsPerWeight.toFixed(1)} bits/weight — this is a heavily quantised build (typical of Q2/Q3). It will be noticeably less accurate and more prone to hallucination; try a Q4_K_M or larger quant if accuracy matters.`);
    } else if (bitsPerWeight < 4.0) {
      quantLevel = "caution";
      notes.push(`~${bitsPerWeight.toFixed(1)} bits/weight — low-ish quantisation. Prefer Q4_K_M (4.5+ b/w) or higher for critical coding work.`);
    }
  }

  return { params, kvGiB, fitsRam, bitsPerWeight, quantLevel, notes };
}