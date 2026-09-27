// Normalized Local Model configuration: one camelCase shape mirrors the
// backend's `ModelInitParams` (all fields optional, `undefined` = runtime
// default / auto), persisted per model path under `localModelConfigs` in
// `settings.json`. Pure helpers here are unit-tested.
//
// Only the model-loading essentials are exposed: context, GPU offload, CPU
// threads, plus max output tokens. Generation sampling is deliberately NOT
// configurable — the engine always uses llama.cpp's own default sampler chain
// so responses are never manipulated by the app. Everything else (batch size,
// flash attention, KV cache, RoPE, llama.cpp argument overrides, …) falls back
// to the engine defaults.

export interface LocalModelConfig {
  /** Layers offloaded to the GPU (0 = CPU only). */
  nGpuLayers?: number;
  /** Requested context (KV cache) size in tokens. */
  contextSize?: number;
  /** Compute threads; unset = all available cores. */
  nThreads?: number;
  /** Generation default: max output tokens. -1 = infinite (context-limited). */
  maxTokens?: number;
}

export const LOCAL_CONFIG_KEYS: (keyof LocalModelConfig)[] = [
  "nGpuLayers",
  "contextSize",
  "nThreads",
  "maxTokens",
];

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