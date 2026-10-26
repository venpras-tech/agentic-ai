import {
  parseQuant,
  estimateRamGb,
  fitStatus,
  parseShard,
  groupByShards,
  GB,
} from "./downloads";

export interface HfResult {
  repoId: string;
  author: string | null;
  description?: string | null;
  pipelineTag?: string | null;
  gated?: boolean | null;
  tags?: string[] | null;
  lastModified?: string | null;
  license?: string | null;
  sha?: string | null;
  likes: number;
  downloads: number;
  files: Array<{ name: string; size: number | null; sha256?: string | null }>;
}

export interface SystemInfo {
  os: string;
  cpu: string;
  cores: number;
  totalRamGb: number;
  availableRamGb: number;
  totalSwapGb: number;
  diskFreeGb: number;
  diskTotalGb: number;
  gpu: { name: string; vramGb: number } | null;
  gpuEnabled: boolean;
}

export type SortKey = "relevance" | "downloads" | "likes" | "name" | "size";

export const STATUS_BANDS = {
  yellow: 0.15,
  red: 0.05,
} as const;

export type StatusLevel = "ok" | "warn" | "crit";

export function levelFor(freeGb: number, totalGb: number): StatusLevel {
  if (!Number.isFinite(freeGb) || !Number.isFinite(totalGb) || totalGb <= 0) return "warn";
  const free = Math.max(0, Math.min(1, freeGb / totalGb));
  if (free <= STATUS_BANDS.red) return "crit";
  if (free <= STATUS_BANDS.yellow) return "warn";
  return "ok";
}

export function ramCapacityLevel(totalGb: number): StatusLevel {
  if (totalGb < 4) return "crit";
  if (totalGb < 8) return "warn";
  return "ok";
}

export const SORT_LABELS: Record<SortKey, string> = {
  relevance: "Relevance",
  downloads: "Downloads",
  likes: "Likes",
  name: "Name",
  size: "Total size",
};

export const POPULAR_FAMILIES = [
  { label: "Llama 3.1 / 3.2", query: "llama 3.1 instruct gguf", hint: "8B for agent work" },
  { label: "Qwen2.5 / Qwen3", query: "qwen2.5 instruct gguf", hint: "Strong at code" },
  { label: "Phi-3 / Phi-4", query: "phi-3 instruct gguf", hint: "Small footprint" },
  { label: "Mistral / Nemo", query: "mistral instruct gguf", hint: "7B–12B general" },
  { label: "Gemma 2 / 3", query: "gemma instruct gguf", hint: "Tool-capable" },
  { label: "Qwen Coder", query: "qwen2.5 coder gguf", hint: "Code edits" },
] as const;

export function repoIsSplitModel(files: Array<{ name: string; size: number | null }>): boolean {
  const ggufs = files.filter((f) => /\.gguf$/i.test(f.name));
  if (ggufs.length < 2) return false;
  return ggufs.every((f) => parseShard(f.name) !== null);
}

export function compactNum(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

export function fitForModel(
  sizeBytes: number,
  sys: SystemInfo | null,
  contextTokens = 8192,
): "fits" | "tight" | "no" | "unknown" {
  if (!sizeBytes || !sys) return "unknown";
  const required = estimateRamGb(sizeBytes, contextTokens);
  const vram = sys.gpuEnabled && sys.gpu ? sys.gpu.vramGb : 0;
  const weightsGb = (sizeBytes * 1.12) / GB;
  const offloaded = Math.min(weightsGb * 0.9, vram * 0.9);
  const ramNeed = Math.max(0.4, required - offloaded);
  return fitStatus(sys.availableRamGb, ramNeed);
}

const QUANT_PREFERENCE = [
  "Q4_K_M",
  "Q5_K_M",
  "Q4_K_S",
  "Q5_K_S",
  "Q4_0",
  "Q5_0",
  "Q3_K_M",
  "Q3_K_S",
  "Q2_K",
];

function quantOrder(q: string): number {
  const idx = QUANT_PREFERENCE.indexOf(q.toUpperCase());
  return idx === -1 ? QUANT_PREFERENCE.length : idx;
}

export interface HubFileRow {
  name: string;
  size: number;
  files: Array<{ name: string; size: number | null }>;
  split: boolean;
}

export function downloadUnits(files: Array<{ name: string; size: number | null }>): HubFileRow[] {
  const groups = groupByShards(files);
  return groups.map((g) => {
    const shard = parseShard(g[0].name);
    if (shard && shard.total > 1) {
      const total = g.reduce((s, f) => s + (f.size || 0), 0);
      return {
        name: g[0].name,
        size: total,
        files: g,
        split: true,
      };
    }
    const f = g[0];
    return { name: f.name, size: f.size || 0, files: g, split: false };
  });
}

export function recommendedUnit(units: HubFileRow[], sys: SystemInfo | null): HubFileRow | null {
  const sizes = units.filter((u) => u.size > 0);
  if (sizes.length === 0) return null;
  const sorted = [...sizes].sort((a, b) => quantOrder(parseQuant(a.name)) - quantOrder(parseQuant(b.name)));
  for (const pref of QUANT_PREFERENCE) {
    const match = sorted.find((u) => parseQuant(u.name) === pref);
    if (match && fitForModel(match.size, sys) !== "no") return match;
  }
  const fitting = sorted.filter((u) => fitForModel(u.size, sys) !== "no");
  return fitting.length > 0 ? fitting[fitting.length - 1] : sorted[0];
}

export function isGatedModel(model: HfResult): boolean {
  return model.gated === true || (model.tags || []).some((t) => t.toLowerCase() === "gated");
}

export function isInstructModel(model: HfResult): boolean {
  const hay = `${model.repoId} ${(model.tags || []).join(" ")}`.toLowerCase();
  if (/[\s._-]base(?:[\s._-]|$)/.test(hay)) return false;
  return /instruct|chat|it\b|assistant/.test(hay);
}

export function fileSizeRange(files: Array<{ name: string; size: number | null }>): { min: number; max: number } {
  const units = downloadUnits(files);
  const sizes = units.map((u) => u.size || 0).filter((s) => s > 0);
  if (sizes.length === 0) return { min: 0, max: 0 };
  return { min: Math.min(...sizes), max: Math.max(...sizes) };
}

export function parseRepoDisplay(repoId: string): { author: string; name: string } {
  const slash = repoId.indexOf("/");
  if (slash === -1) return { author: "", name: repoId };
  return { author: repoId.slice(0, slash), name: repoId.slice(slash + 1) };
}

/** LM Studio accepts keywords, `user/model`, or a full Hugging Face URL. */
export function normalizeHubSearchQuery(raw: string): string {
  const t = raw.trim();
  if (!t) return "";
  const fromUrl = t.match(/huggingface\.co\/([^/?#]+(?:\/[^/?#]+)?)/i);
  if (fromUrl) return decodeURIComponent(fromUrl[1]);
  return t;
}

export function compatLevel(
  availableGb: number,
  files: Array<{ name: string; size: number | null }>,
): "ok" | "bad" | "unknown" {
  if (!availableGb) return "unknown";
  const units = downloadUnits(files);
  const sizes = units.map((u) => u.size || 0).filter((s) => s > 0);
  if (sizes.length === 0) return "unknown";
  const min = Math.min(...sizes);
  if (min <= 0) return "unknown";
  const ram = estimateRamGb(min, 8192);
  const fit = fitStatus(availableGb, ram);
  return fit === "no" ? "bad" : fit === "unknown" ? "unknown" : "ok";
}
