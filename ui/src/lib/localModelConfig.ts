// Normalized Local Model configuration: one camelCase shape mirrors the
// backend's `ModelInitParams` (all fields optional, `undefined` = runtime
// default / auto), persisted per model path under `localModelConfigs` in
// `settings.json`. Pure helpers here are unit-tested.
//
// The essentials (context, GPU offload, CPU threads, max output tokens) are
// edited on the LLM page; the advanced llama.cpp internals (batch size,
// flash attention, KV-cache quantization, memory mapping, RoPE, raw argument
// overrides, …) are edited on the llama.cpp page. Generation sampling is
// deliberately NOT configurable — the engine always uses llama.cpp's own
// default sampler chain so responses are never manipulated by the app.

export interface LocalModelConfig {
  /** Layers offloaded to the GPU (0 = CPU only). */
  nGpuLayers?: number;
  /** Requested context (KV cache) size in tokens. */
  contextSize?: number;
  /** Compute threads; unset = all available cores. */
  nThreads?: number;
  /** Generation default: max output tokens. -1 = infinite (context-limited). */
  maxTokens?: number;
  // ---- advanced llama.cpp internals (edited on the llama.cpp page) ----
  /** How many parallel engine workers (each with its own context). */
  nWorkers?: number;
  /** Evaluation batch size (llama.cpp `n_batch`). */
  nBatch?: number;
  /** Physical batch size (llama.cpp `n_ubatch`). */
  nUbatch?: number;
  /** Unified KV cache (uKVC). */
  unifiedKv?: boolean;
  /** Flash attention. Unset = runtime auto. */
  flashAttn?: boolean;
  /** Offload the KV cache to GPU memory (`--no-kv-offload` disables). */
  offloadKqv?: boolean;
  /** Memory-map the weights (`--mmap`). */
  useMmap?: boolean;
  /** Lock the model in memory (`--mlock`). */
  useMlock?: boolean;
  /** RoPE frequency base override. */
  ropeFreqBase?: number;
  /** RoPE frequency scale override. */
  ropeFreqScale?: number;
  /** K-cache quantization type (e.g. `Q8_0`). */
  cacheTypeK?: string;
  /** V-cache quantization type (e.g. `Q8_0`). */
  cacheTypeV?: string;
  /** Seed used for local generations when the request carries none. */
  seed?: number;
  /** Raw llama.cpp argument overrides (space separated). */
  llamaArgs?: string;
}

export const LOCAL_CONFIG_KEYS: (keyof LocalModelConfig)[] = [
  "nGpuLayers",
  "contextSize",
  "nThreads",
  "maxTokens",
  "nWorkers",
  "nBatch",
  "nUbatch",
  "unifiedKv",
  "flashAttn",
  "offloadKqv",
  "useMmap",
  "useMlock",
  "ropeFreqBase",
  "ropeFreqScale",
  "cacheTypeK",
  "cacheTypeV",
  "seed",
  "llamaArgs",
];

/** Keys whose value is a boolean (so sanitizing must not coerce them to numbers). */
const BOOLEAN_CONFIG_KEYS = new Set<keyof LocalModelConfig>([
  "unifiedKv",
  "flashAttn",
  "offloadKqv",
  "useMmap",
  "useMlock",
]);

/** Keys whose value is a string. */
const STRING_CONFIG_KEYS = new Set<keyof LocalModelConfig>([
  "cacheTypeK",
  "cacheTypeV",
  "llamaArgs",
]);

const DEFAULT_CONFIG_KEY = "default";

/** The active config for a loaded local model: per-path entry, else "default".
 * Backend round-trips unset fields as `null`, so the result is sanitized
 * (`null`/`""`/`NaN` dropped) — callers always see numbers-or-undefined. */
export function activeLocalConfig(
  configs: Record<string, Partial<LocalModelConfig>>,
  path: string | null,
): Partial<LocalModelConfig> | undefined {
  const raw =
    (path && configs[path] ? configs[path] : undefined) ??
    configs[DEFAULT_CONFIG_KEY];
  if (!raw) return undefined;
  const clean = sanitizeLocalConfig(raw);
  return Object.keys(clean).length ? clean : undefined;
}

/** Drop backend `null`s, empty strings and NaN so unset stays unset.
 * Accepts the raw IPC payload (which may carry `null`s) as well as drafts. */
export function sanitizeLocalConfig<T extends Partial<LocalModelConfig>>(
  cfg: Record<string, unknown>,
): Partial<LocalModelConfig> {
  const out: Partial<LocalModelConfig> = {};
  for (const key of LOCAL_CONFIG_KEYS) {
    const v = (cfg as Record<string, unknown>)[key];
    if (v === undefined || v === null) continue;
    if (BOOLEAN_CONFIG_KEYS.has(key)) {
      if (typeof v === "boolean") (out as Record<string, unknown>)[key] = v;
      continue;
    }
    if (STRING_CONFIG_KEYS.has(key)) {
      if (typeof v === "string" && v.trim() !== "") {
        (out as Record<string, unknown>)[key] = v;
      }
      continue;
    }
    if (typeof v === "string") {
      if (v.trim() === "") continue;
      const n = Number(v);
      if (Number.isNaN(n)) continue;
      (out as Record<string, unknown>)[key] = n;
      continue;
    }
    if (typeof v === "number") {
      if (Number.isNaN(v)) continue;
      (out as Record<string, unknown>)[key] = v;
    }
  }
  return out as T;
}

/** Overlay `patch` (defined fields only) on top of `base`. */
export function mergeLocalConfig(
  base: Partial<LocalModelConfig> | undefined,
  patch: Partial<LocalModelConfig> | undefined,
): Partial<LocalModelConfig> {
  const out: Partial<LocalModelConfig> = { ...(sanitizeLocalConfig(base ?? {})) };
  if (!patch) return out;
  const clean = sanitizeLocalConfig(patch as Record<string, unknown>);
  for (const key of LOCAL_CONFIG_KEYS) {
    const v = clean[key];
    if (v !== undefined) (out as Record<keyof LocalModelConfig, unknown>)[key] = v;
  }
  return out;
}

/** Keep only the defined keys (drop `undefined`/`null`/`""`/`NaN`) so nothing redundant is persisted. */
export function normalizedConfig<T extends Partial<LocalModelConfig>>(c: T): T {
  return sanitizeLocalConfig(c as Record<string, unknown>) as T;
}

/**
 * Overlay `patch` on top of `base`, where a key the caller explicitly set to
 * `null` (or `""`) is *removed* rather than ignored.
 *
 * `mergeLocalConfig` cannot unset a value — a sanitized patch just omits it,
 * which leaves the previous value in place. Forms that let a user blank a field
 * back to "auto/engine default" need that removal, so they call this instead.
 */
export function applyClearedConfig(
  base: Partial<LocalModelConfig> | undefined,
  patch: Record<string, unknown>,
): Partial<LocalModelConfig> {
  const out: Partial<LocalModelConfig> = { ...(sanitizeLocalConfig(base ?? {})) };
  const target = out as Record<string, unknown>;
  for (const key of LOCAL_CONFIG_KEYS) {
    if (!(key in patch)) continue;
    const raw = patch[key];
    const cleared =
      raw === null || raw === undefined || (typeof raw === "string" && raw.trim() === "");
    if (cleared) {
      delete target[key];
      continue;
    }
    const clean = sanitizeLocalConfig({ [key]: raw } as Record<string, unknown>);
    const v = clean[key];
    if (v !== undefined) target[key] = v;
  }
  return out;
}

const int = (v: unknown, label: string): number | null => {
  if (v === undefined || v === null) return null;
  if (typeof v === "string" && v.trim() === "") return null;
  const n = typeof v === "number" ? v : Number(v);
  if (Number.isNaN(n)) return null;
  if (!Number.isInteger(n)) throw new Error(`${label} must be a whole number`);
  return n;
};

const err = (msg: string) => `${msg}`;

/**
 * Validate a config the same way the backend does, returning user-facing
 * messages. Empty array = valid.
 */
export function validateLocalModelConfig(cfg: Partial<LocalModelConfig>): string[] {
  const errors: string[] = [];
  try {
    const gpu = int(cfg.nGpuLayers, "GPU layers");
    if (gpu !== null && (gpu < 0 || gpu > 16384)) {
      errors.push(err(`GPU layers must be between 0 and 16384 (got ${gpu})`));
    }
    const threads = int(cfg.nThreads, "Threads");
    if (threads !== null && (threads < 1 || threads > 1024)) {
      errors.push(err(`CPU thread pool size must be between 1 and 1024 (got ${threads})`));
    }
    const maxTokens = int(cfg.maxTokens, "Max tokens");
    if (maxTokens !== null && maxTokens !== -1 && (maxTokens < 1 || maxTokens > 1_048_576)) {
      errors.push(err(`Max tokens must be -1 (infinite) or between 1 and 1048576 (got ${maxTokens})`));
    }
  } catch (e) {
    errors.push((e as Error).message);
  }
  return errors;
}

export { DEFAULT_CONFIG_KEY };