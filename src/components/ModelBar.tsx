import { useCallback, useEffect, useRef, useState } from "react";

import { api } from "../lib/ipc";
import type {
  GenParams,
  ModelInfo,
  RemoteModelConfig,
  RemoteProviderPreset,
} from "../types";

interface ModelBarProps {
  model: ModelInfo | null;
  loading: boolean;
  progress: number | null;
  isStreaming: boolean;
  params: GenParams;
  initialRemote?: RemoteModelConfig | null;
  onParamsChange: (patch: Partial<GenParams>) => void;
  onLoad: () => void;
  onUnload: () => void;
  onCancel: () => void;
  onConnectRemote: (config: RemoteModelConfig) => void;
}

const PROVIDERS: RemoteProviderPreset[] = [
  {
    id: "openai",
    label: "OpenAI",
    baseUrl: "https://api.openai.com/v1",
    apiKeyPlaceholder: "sk-…",
    apiKeyRequired: true,
    defaultModel: "gpt-4o-mini",
    hint: "Official OpenAI API.",
  },
  {
    id: "anthropic",
    label: "Anthropic (Claude)",
    baseUrl: "https://api.anthropic.com/v1",
    apiKeyPlaceholder: "sk-ant-…",
    apiKeyRequired: true,
    defaultModel: "claude-sonnet-4-5",
    hint: "Native Messages API. Claude excels at structured tool use.",
  },
  {
    id: "openrouter",
    label: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    apiKeyPlaceholder: "sk-or-…",
    apiKeyRequired: true,
    defaultModel: "anthropic/claude-sonnet-4-5",
    hint: "One key, hundreds of models across labs.",
  },
  {
    id: "google",
    label: "Google Gemini",
    baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
    apiKeyPlaceholder: "AIza…",
    apiKeyRequired: true,
    defaultModel: "gemini-2.0-flash",
    hint: "OpenAI-compatible gateway to Gemini models.",
  },
  {
    id: "ollama",
    label: "Ollama (local)",
    baseUrl: "http://localhost:11434/v1",
    apiKeyPlaceholder: "No key needed",
    apiKeyRequired: false,
    defaultModel: "qwen3:8b",
    hint: "Local server — no API key required. 8B+ models work best for agent tasks.",
  },
  {
    id: "lmstudio",
    label: "LM Studio (local)",
    baseUrl: "http://localhost:1234/v1",
    apiKeyPlaceholder: "No key needed",
    apiKeyRequired: false,
    defaultModel: "",
    hint: "Local server — no API key required.",
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    baseUrl: "https://api.deepseek.com/v1",
    apiKeyPlaceholder: "sk-…",
    apiKeyRequired: true,
    defaultModel: "deepseek-chat",
    hint: "Chat + reasoner models.",
  },
  {
    id: "xai",
    label: "xAI (Grok)",
    baseUrl: "https://api.x.ai/v1",
    apiKeyPlaceholder: "xai-…",
    apiKeyRequired: true,
    defaultModel: "grok-4",
    hint: "Grok family.",
  },
  {
    id: "groq",
    label: "Groq",
    baseUrl: "https://api.groq.com/openai/v1",
    apiKeyPlaceholder: "gsk_…",
    apiKeyRequired: true,
    defaultModel: "llama-3.3-70b-versatile",
    hint: "Fast hosted inference.",
  },
  {
    id: "mistral",
    label: "Mistral",
    baseUrl: "https://api.mistral.ai/v1",
    apiKeyPlaceholder: "…",
    apiKeyRequired: true,
    defaultModel: "mistral-large-latest",
    hint: "Mistral model family.",
  },
  {
    id: "custom",
    label: "Custom (OpenAI-compatible)",
    baseUrl: "",
    apiKeyPlaceholder: "Optional key…",
    apiKeyRequired: false,
    defaultModel: "",
    hint: "Any server exposing /chat/completions and /models (vLLM, LiteLLM, proxies…).",
  },
];

const DEFAULT_PROVIDER = PROVIDERS[0];

function formatBytes(n: number): string {
  if (n >= 1 << 30) return `${(n / (1 << 30)).toFixed(1)} GB`;
  if (n >= 1 << 20) return `${(n / (1 << 20)).toFixed(0)} MB`;
  return `${n} B`;
}

export default function ModelBar(props: ModelBarProps) {
  const { model, loading, progress, isStreaming, params, initialRemote, onParamsChange, onLoad, onUnload, onCancel, onConnectRemote } = props;
  const [showRemote, setShowRemote] = useState(false);
  const [providerId, setProviderId] = useState(
    initialRemote?.provider ?? DEFAULT_PROVIDER.id,
  );
  const [remote, setRemote] = useState<RemoteModelConfig>({
    provider: initialRemote?.provider ?? DEFAULT_PROVIDER.id,
    baseUrl: initialRemote?.baseUrl ?? DEFAULT_PROVIDER.baseUrl,
    apiKey: "",
    model: initialRemote?.model ?? DEFAULT_PROVIDER.defaultModel,
  });
  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState<string | null>(null);

  const configRef = useRef(remote);
  configRef.current = remote;

  const appliedInitial = useRef(false);
  useEffect(() => {
    if (appliedInitial.current || !initialRemote) return;
    appliedInitial.current = true;
    setProviderId(initialRemote.provider || DEFAULT_PROVIDER.id);
    setRemote({
      provider: initialRemote.provider || DEFAULT_PROVIDER.id,
      baseUrl: initialRemote.baseUrl,
      apiKey: "",
      model: initialRemote.model,
    });
  }, [initialRemote]);

  const isRemote = model?.architecture === "remote-api";
  const preset =
    PROVIDERS.find((p) => p.id === providerId) ?? PROVIDERS[PROVIDERS.length - 1];

  const fetchModels = useCallback(async (cfg: RemoteModelConfig) => {
    if (!cfg.baseUrl.trim()) {
      setModels([]);
      return;
    }
    setModelsLoading(true);
    setModelsError(null);
    try {
      const list = await api.listRemoteModels({
        provider: cfg.provider,
        baseUrl: cfg.baseUrl.trim(),
        apiKey: cfg.apiKey.trim(),
      });
      setModels(list);
      setRemote((r) => ({ ...r, model: list.includes(r.model) ? r.model : r.model || list[0] }));
    } catch (e) {
      setModels([]);
      setModelsError(String(e));
    } finally {
      setModelsLoading(false);
    }
  }, []);

  // Debounced auto-fetch: whenever base URL, key or provider changes, refresh
  // the model dropdown — unless the provider needs a key we don't have yet.
  useEffect(() => {
    if (!showRemote || !remote.baseUrl.trim()) return;
    const currentPreset = PROVIDERS.find((p) => p.id === remote.provider);
    if (currentPreset?.apiKeyRequired && !remote.apiKey.trim()) return;
    const timer = setTimeout(() => void fetchModels(configRef.current), 650);
    return () => clearTimeout(timer);
  }, [remote.baseUrl, remote.apiKey, remote.provider, showRemote, fetchModels]);

  const selectProvider = (id: string) => {
    const p = PROVIDERS.find((x) => x.id === id) ?? PROVIDERS[PROVIDERS.length - 1];
    setProviderId(id);
    setModels([]);
    setModelsError(null);
    setRemote((r) => ({
      ...r,
      provider: id,
      baseUrl: p.baseUrl,
      model: p.defaultModel,
    }));
  };

  const submitRemote = () => {
    if (!remote.baseUrl.trim() || !remote.model.trim()) return;
    if (preset.apiKeyRequired && !remote.apiKey.trim()) return;
    setShowRemote(false);
    onConnectRemote({
      ...remote,
      baseUrl: remote.baseUrl.trim(),
      model: remote.model.trim(),
    });
  };

  const canConnect =
    remote.baseUrl.trim() !== "" &&
    remote.model.trim() !== "" &&
    (!preset.apiKeyRequired || remote.apiKey.trim() !== "");

  return (
    <div className="flex h-11 shrink-0 items-center gap-3 border-b border-border bg-panel px-3">
      {model ? (
        <>
          <div className="flex min-w-0 flex-col">
            <span className="truncate text-[12px] font-semibold text-ink">{model.name}</span>
            <span className="text-[10px] text-zinc-500">
              <span
                className={`mr-1.5 rounded px-1 py-px text-[9px] font-semibold uppercase ${
                  isRemote ? "bg-violet-500/20 text-violet-300" : "bg-emerald-500/20 text-emerald-300"
                }`}
              >
                {isRemote ? "remote" : "local"}
              </span>
              {model.architecture} · ctx {model.contextSize}
              {!isRemote &&
                (model.nParams >= 1_000_000_000
                  ? ` · ${(model.nParams / 1_000_000_000).toFixed(1)}B`
                  : ` · ${(model.nParams / 1_000_000).toFixed(0)}M`)}
              {!isRemote && ` · ${formatBytes(model.sizeBytes)}`}
            </span>
          </div>
          <button
            onClick={onUnload}
            className="rounded border border-border px-2 py-1 text-[11px] text-zinc-400 hover:border-zinc-600 hover:text-zinc-200"
          >
            Unload
          </button>
          <div className="mx-1 h-6 w-px bg-border" />
        </>
      ) : (
        <div className="flex items-center gap-2">
          <button
            onClick={onLoad}
            disabled={loading}
            className="rounded bg-accent px-3 py-1.5 text-[12px] font-semibold text-black hover:bg-cyan-300 disabled:opacity-60"
          >
            {loading ? "Loading model…" : "Load GGUF Model"}
          </button>
          <button
            onClick={() => setShowRemote((v) => !v)}
            className="rounded border border-border px-3 py-1.5 text-[12px] text-zinc-400 hover:border-zinc-500 hover:text-zinc-200"
          >
            Remote…
          </button>
        </div>
      )}

      {showRemote && !model && (
        <div className="absolute left-3 top-11 z-30 flex w-[28rem] flex-col gap-2 rounded-md border border-border bg-panel-2 p-3 shadow-2xl">
          <div className="flex items-center justify-between">
            <span className="text-[11px] font-semibold uppercase tracking-wide text-zinc-400">
              Connect a model provider
            </span>
            <button
              onClick={() => setShowRemote(false)}
              aria-label="Close"
              title="Close"
              className="rounded p-1 text-zinc-500 hover:bg-panel hover:text-zinc-200"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <path d="M18 6 6 18M6 6l12 12" />
              </svg>
            </button>
          </div>

          <label className="flex flex-col gap-1 text-[10px] text-zinc-500">
            Provider
            <select
              value={providerId}
              onChange={(e) => selectProvider(e.target.value)}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            >
              {PROVIDERS.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </label>

          <label className="flex flex-col gap-1 text-[10px] text-zinc-500">
            Base URL
            <input
              value={remote.baseUrl}
              onChange={(e) => setRemote((r) => ({ ...r, baseUrl: e.target.value }))}
              placeholder="https://api.openai.com/v1"
              spellCheck={false}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[10px] text-zinc-500">
            API key {preset.apiKeyRequired ? "" : "(optional for local servers)"}
            <input
              type="password"
              value={remote.apiKey}
              onChange={(e) => setRemote((r) => ({ ...r, apiKey: e.target.value }))}
              placeholder={preset.apiKeyPlaceholder}
              spellCheck={false}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[10px] text-zinc-500">
            Model
            <div className="flex items-center gap-1.5">
              <input
                list="remote-model-options"
                value={remote.model}
                onChange={(e) => setRemote((r) => ({ ...r, model: e.target.value }))}
                placeholder="Pick from list or type an id"
                spellCheck={false}
                className="w-full rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
              />
              <button
                onClick={() => void fetchModels(configRef.current)}
                disabled={!remote.baseUrl.trim() || modelsLoading}
                title="Refresh model list"
                className="shrink-0 rounded border border-border px-2 py-1 text-[11px] text-zinc-400 hover:border-zinc-500 hover:text-zinc-200 disabled:opacity-40"
              >
                {modelsLoading ? "…" : "↻"}
              </button>
            </div>
            <datalist id="remote-model-options">
              {models.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
            <span className="text-[9px] text-zinc-600">
              {modelsLoading
                ? "Fetching available models…"
                : models.length > 0
                  ? `${models.length} model${models.length === 1 ? "" : "s"} available`
                  : modelsError
                    ? `No models listed: ${modelsError}`
                    : "Type a model id, or press ↻ to load the list."}
            </span>
          </label>

          <div className="mt-1 flex items-end justify-between gap-3">
            <p className="max-w-64 text-[10px] leading-snug text-zinc-600">{preset.hint}</p>
            <button
              onClick={submitRemote}
              disabled={!canConnect}
              className="rounded bg-accent px-3 py-1 text-[11px] font-semibold text-black hover:bg-cyan-300 disabled:opacity-40"
            >
              Connect
            </button>
          </div>
          <p className="text-[9px] leading-snug text-zinc-600">
            Keys are held in memory only — never written to disk.
          </p>
        </div>
      )}

      {loading && progress !== null && (
        <div className="flex min-w-40 flex-1 items-center gap-2">
          <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-panel-2">
            <div
              className="h-full rounded-full bg-accent transition-[width] duration-200"
              style={{ width: `${Math.round(progress * 100)}%` }}
            />
          </div>
          <span className="w-10 text-right text-[11px] tabular-nums text-zinc-400">
            {Math.round(progress * 100)}%
          </span>
        </div>
      )}

      {model && !loading && (
        <div className="flex flex-1 items-center justify-end gap-4 text-[11px] text-zinc-400">
          <label className="flex items-center gap-1.5">
            ctx
            <select
              value={params.contextSize}
              onChange={(e) => onParamsChange({ contextSize: Number(e.target.value) })}
              className="rounded border border-border bg-panel-2 px-1.5 py-0.5 text-[11px] outline-none"
            >
              {[2048, 4096, 8192, 16384].map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-1.5">
            temp
            <input
              type="number"
              min={0}
              max={2}
              step={0.05}
              value={params.temperature}
              onChange={(e) => onParamsChange({ temperature: Number(e.target.value) })}
              className="spin-none w-14 rounded border border-border bg-panel-2 px-1.5 py-0.5 outline-none"
            />
          </label>
          <label className="flex items-center gap-1.5">
            top-p
            <input
              type="number"
              min={0}
              max={1}
              step={0.05}
              value={params.topP}
              onChange={(e) => onParamsChange({ topP: Number(e.target.value) })}
              className="spin-none w-14 rounded border border-border bg-panel-2 px-1.5 py-0.5 outline-none"
            />
          </label>
          <label className="flex items-center gap-1.5">
            max tok
            <input
              type="number"
              min={16}
              max={16384}
              step={16}
              value={params.maxTokens}
              onChange={(e) => onParamsChange({ maxTokens: Number(e.target.value) })}
              className="spin-none w-20 rounded border border-border bg-panel-2 px-1.5 py-0.5 outline-none"
            />
          </label>
          <details className="relative">
            <summary className="cursor-pointer list-none rounded px-1.5 text-zinc-500 hover:text-zinc-300">
              adv
            </summary>
            <div className="absolute right-0 top-6 z-20 flex w-56 flex-col gap-2 rounded-md border border-border bg-panel-2 p-3 shadow-xl">
              <label className="flex items-center justify-between gap-2">
                threads
                <input
                  type="number"
                  min={1}
                  max={256}
                  value={params.nThreads}
                  onChange={(e) => onParamsChange({ nThreads: Number(e.target.value) })}
                  className="spin-none w-16 rounded border border-border bg-panel px-1.5 py-0.5 outline-none"
                />
              </label>
              <label className="flex items-center justify-between gap-2">
                gpu layers
                <input
                  type="number"
                  min={0}
                  max={999}
                  value={params.nGpuLayers}
                  onChange={(e) => onParamsChange({ nGpuLayers: Number(e.target.value) })}
                  className="spin-none w-16 rounded border border-border bg-panel px-1.5 py-0.5 outline-none"
                />
              </label>
              <p className="text-[10px] leading-snug text-zinc-600">
                GPU/context settings take effect on the next model load. CPU-only
                build by default.
              </p>
            </div>
          </details>
          {isStreaming && (
            <button
              onClick={onCancel}
              className="rounded border border-red-400/40 px-2.5 py-1 text-[11px] font-medium text-red-300 hover:bg-red-500/10"
            >
              ■ Stop
            </button>
          )}
        </div>
      )}
    </div>
  );
}
