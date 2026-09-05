import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { api } from "../lib/ipc";
import type {
  DownloadedModel,
  GenParams,
  HfModel,
  HubDownloadProgress,
  ModelInfo,
  RemoteModelConfig,
  RemoteProviderPreset,
} from "../types";

interface ModelBarProps {
  model: ModelInfo | null;
  /** Absolute path of the currently loaded GGUF (local models only). */
  path?: string | null;
  /** Most recent local GGUF path — stays visible while nothing is loaded. */
  lastPath?: string | null;
  loading: boolean;
  progress: number | null;
  isStreaming: boolean;
  params: GenParams;
  initialRemote?: RemoteModelConfig | null;
  recentModels?: string[];
  onParamsChange: (patch: Partial<GenParams>) => void;
  onLoad: () => void;
  onUnload: () => void;
  onSwitchModel: (path: string) => void;
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

function ProgressBar({
  p,
  downloaded,
  onSwitchModel,
}: {
  p: HubDownloadProgress;
  downloaded: DownloadedModel[];
  onSwitchModel: (path: string) => void;
}) {
  const done = p.done || p.cancelled || p.error != null;
  const frac =
    p.totalBytes && p.totalBytes > 0 ? (p.receivedBytes ?? 0) / p.totalBytes : null;
  const local = downloaded.find(
    (m) => m.repoId === p.repoId && m.fileName === p.file,
  );
  return (
    <div className="mt-0.5 flex items-center gap-1.5">
      <div className="h-1 flex-1 overflow-hidden rounded-full bg-panel-2">
        <div
          className="h-full rounded-full bg-accent transition-[width] duration-200"
          style={{ width: `${Math.round((frac ?? 0) * 100)}%` }}
        />
      </div>
      <span className="shrink-0 text-[9px] tabular-nums text-zinc-400">
        {p.error ? "error" : p.cancelled ? "cancelled" : frac != null ? `${Math.round(frac * 100)}%` : "…"}
      </span>
      {done && !p.error && !p.cancelled && local && (
        <button
          onClick={() => onSwitchModel(local.path)}
          className="shrink-0 rounded border border-accent/40 px-1.5 py-px text-[10px] text-accent hover:bg-accent/10"
        >
          Load
        </button>
      )}
    </div>
  );
}

export default function ModelBar(props: ModelBarProps) {
  const { model, path, lastPath, loading, progress, isStreaming, params, initialRemote, recentModels = [], onParamsChange, onLoad, onUnload, onSwitchModel, onCancel, onConnectRemote } = props;

  const fileName = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
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
  const [showModelSwitcher, setShowModelSwitcher] = useState(false);
  const [downloaded, setDownloaded] = useState<DownloadedModel[]>([]);
  // Hub search + download surfaced in-bar (mirrors Settings → Models tab).
  const [hubQuery, setHubQuery] = useState("");
  const [hubResults, setHubResults] = useState<HfModel[] | null>(null);
  const [hubBusy, setHubBusy] = useState(false);
  const [hubError, setHubError] = useState<string | null>(null);
  const [hubProgress, setHubProgress] = useState<Record<string, HubDownloadProgress>>({});

  const rootRef = useRef<HTMLDivElement | null>(null);
  const fetchSeqRef = useRef(0);

  const configRef = useRef(remote);
  configRef.current = remote;

  useEffect(() => {
    if (!showModelSwitcher && !showRemote) return;
    const onPointer = (e: MouseEvent | TouchEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setShowModelSwitcher(false);
        setShowRemote(false);
      }
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setShowModelSwitcher(false);
        setShowRemote(false);
      }
    };
    document.addEventListener("mousedown", onPointer);
    document.addEventListener("touchstart", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onPointer);
      document.removeEventListener("touchstart", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [showModelSwitcher, showRemote]);

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

  // Load downloaded models for the switcher dropdown.
  useEffect(() => {
    if (!showModelSwitcher) return;
    api.listDownloadedModels().then(setDownloaded).catch(() => setDownloaded([]));
  }, [showModelSwitcher]);

  const refreshDownloaded = () => {
    api.listDownloadedModels().then(setDownloaded).catch(() => setDownloaded([]));
  };

  // Live hub download progress while the switcher is open.
  useEffect(() => {
    if (!showModelSwitcher) return;
    const unlisten = listen<HubDownloadProgress>(
      "hf-download-progress",
      (e) => {
        const p = e.payload;
        setHubProgress((prev) => ({ ...prev, [`${p.repoId}::${p.file}`]: p }));
        if (p.done || p.cancelled || p.error) refreshDownloaded();
      },
    );
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [showModelSwitcher]);

  const searchHub = useCallback(async (query: string) => {
    if (!query.trim()) {
      setHubResults(null);
      return;
    }
    setHubBusy(true);
    setHubError(null);
    try {
      setHubResults(await api.hfSearch(query.trim(), 12));
    } catch (e) {
      setHubError(String(e));
      setHubResults(null);
    } finally {
      setHubBusy(false);
    }
  }, []);

  // Debounced hub search as the user types in the in-bar search box.
  useEffect(() => {
    if (!showModelSwitcher) return;
    const timer = setTimeout(() => void searchHub(hubQuery), 400);
    return () => clearTimeout(timer);
  }, [hubQuery, showModelSwitcher, searchHub]);

  const startHubDownload = (repoId: string, file: string) => {
    setHubError(null);
    api.hfDownloadModel(repoId, file).catch((e) => setHubError(String(e)));
  };
  const preset =
    PROVIDERS.find((p) => p.id === providerId) ?? PROVIDERS[PROVIDERS.length - 1];

  const fetchModels = useCallback(async (cfg: RemoteModelConfig) => {
    if (!cfg.baseUrl.trim()) {
      setModels([]);
      return;
    }
    const seq = ++fetchSeqRef.current;
    setModelsLoading(true);
    setModelsError(null);
    try {
      const list = await api.listRemoteModels({
        provider: cfg.provider,
        baseUrl: cfg.baseUrl.trim(),
        apiKey: cfg.apiKey.trim(),
      });
      if (seq !== fetchSeqRef.current) return;
      setModels(list);
      setRemote((r) => ({ ...r, model: list.includes(r.model) ? r.model : r.model || list[0] }));
    } catch (e) {
      if (seq !== fetchSeqRef.current) return;
      setModels([]);
      setModelsError(String(e));
    } finally {
      if (seq === fetchSeqRef.current) setModelsLoading(false);
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
    <div ref={rootRef} className="flex min-h-11 shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-border bg-panel px-3">
      {model ? (
        <>
          <div className="flex min-w-0 flex-col">
            <span className="truncate text-[12px] font-semibold text-ink">{model.name}</span>
            <span className="min-w-0 truncate text-[10px] text-zinc-500">
              <span
                className={`mr-1.5 rounded px-1 py-px text-[9px] font-semibold uppercase ${
                  isRemote ? "bg-violet-500/20 text-violet-600" : "bg-emerald-500/20 text-emerald-600"
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
            className="rounded border border-border px-2 py-1 text-[11px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800"
          >
            Unload
          </button>
          <div className="relative">
            <button
              onClick={() => setShowModelSwitcher(!showModelSwitcher)}
              aria-label="Switch model"
              className="rounded border border-border px-2 py-1 text-[11px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800"
            >
              Switch ▾
            </button>
            {showModelSwitcher && (
              <div className="absolute left-0 top-full z-30 mt-1 w-80 max-h-72 overflow-auto rounded-md border border-border bg-panel-2 p-2 shadow-xl">
                <div className="mb-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
                  Switch model
                </div>
                {recentModels.length > 0 && (
                  <>
                    <div className="mb-1 text-[9px] text-zinc-400">Recent</div>
                    {recentModels.map((p) => {
                      const name = p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() ?? p;
                      const isCurrent = path === p;
                      return (
                        <button
                          key={p}
                          disabled={isCurrent || isStreaming}
                          onClick={() => { setShowModelSwitcher(false); onSwitchModel(p); }}
                          className={`flex w-full items-center gap-2 rounded px-2 py-1 text-left text-[11px] hover:bg-zinc-100 ${isCurrent ? "font-semibold text-accent" : "text-zinc-700"} disabled:opacity-40`}
                          title={p}
                        >
                          <span className="truncate flex-1">{name}</span>
                          {isCurrent && <span className="text-[9px] text-accent">active</span>}
                        </button>
                      );
                    })}
                    <div className="my-1 border-t border-border" />
                  </>
                )}
                {downloaded.length > 0 && (
                  <>
                    <div className="mb-1 text-[9px] text-zinc-400">Downloaded</div>
                    {downloaded.map((m) => {
                      const isCurrent = path === m.path;
                      return (
                        <button
                          key={m.path}
                          disabled={isCurrent || isStreaming}
                          onClick={() => { setShowModelSwitcher(false); onSwitchModel(m.path); }}
                          className={`flex w-full items-center gap-2 rounded px-2 py-1 text-left text-[11px] hover:bg-zinc-100 ${isCurrent ? "font-semibold text-accent" : "text-zinc-700"} disabled:opacity-40`}
                          title={m.path}
                        >
                          <span className="truncate flex-1">{m.fileName}</span>
                          <span className="shrink-0 text-[9px] text-zinc-400">{formatBytes(m.sizeBytes)}</span>
                          {isCurrent && <span className="text-[9px] text-accent">active</span>}
                        </button>
                      );
                    })}
                  </>
                )}
                {downloaded.length === 0 && recentModels.length === 0 && (
                  <p className="px-2 py-2 text-[11px] text-zinc-400">
                    No downloaded models found. Use the Models tab to download from HuggingFace.
                  </p>
                )}
                <button
                  onClick={() => { setShowModelSwitcher(false); onLoad(); }}
                  className="mt-2 flex w-full items-center justify-center gap-1 rounded border border-border px-2 py-1 text-[11px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800"
                >
                  + Load from file…
                </button>
                <div className="my-2 border-t border-border" />
                <div className="mb-1 text-[9px] text-zinc-400">Model Hub (HuggingFace)</div>
                <input
                  value={hubQuery}
                  onChange={(e) => setHubQuery(e.target.value)}
                  placeholder="Search GGUF models…"
                  spellCheck={false}
                  className="w-full rounded border border-border bg-panel px-2 py-1 text-[11px] text-ink outline-none focus:border-accent/60"
                />
                {hubBusy && (
                  <p className="mt-1 px-0.5 text-[10px] text-zinc-400">Searching hub…</p>
                )}
                {hubError && (
                  <p className="mt-1 px-0.5 text-[10px] text-red-500">{hubError}</p>
                )}
                {hubResults && hubResults.length > 0 && (
                  <div className="mt-1 max-h-44 overflow-auto">
                    {hubResults.map((m) => {
                      const gguf = m.files.find((f) => f.name.endsWith(".gguf") && !f.name.includes(".json"));
                      if (!gguf) return null;
                      return (
                        <div key={m.repoId} className="flex flex-col gap-0.5 rounded px-1 py-1 hover:bg-zinc-100">
                          <div className="flex items-center gap-1.5">
                            <span className="min-w-0 flex-1 truncate text-[11px] text-zinc-700" title={m.repoId}>
                              {m.repoId}
                            </span>
                            <span className="shrink-0 text-[9px] text-zinc-400">
                              {m.downloads.toLocaleString()} dl
                            </span>
                          </div>
                          <div className="flex items-center gap-1.5">
                            <span className="min-w-0 flex-1 truncate text-[10px] text-zinc-400">
                              {gguf.name} · {gguf.size != null ? formatBytes(gguf.size) : "?"}
                            </span>
                            <button
                              disabled={isStreaming}
                              onClick={() => startHubDownload(m.repoId, gguf.name)}
                              className="shrink-0 rounded border border-border px-1.5 py-px text-[10px] text-zinc-600 hover:border-accent/50 hover:text-accent disabled:opacity-40"
                            >
                              Download
                            </button>
                          </div>
                          {hubProgress[`${m.repoId}::${gguf.name}`] && (
                            <ProgressBar p={hubProgress[`${m.repoId}::${gguf.name}`]} downloaded={downloaded} onSwitchModel={onSwitchModel} />
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}
                {hubResults && hubResults.length === 0 && !hubBusy && hubQuery.trim() !== "" && (
                  <p className="mt-1 px-0.5 text-[10px] text-zinc-400">No matching GGUF models.</p>
                )}
              </div>
            )}
          </div>
          {path && (
            <span
              title={path}
              className="max-w-72 min-w-0 truncate rounded bg-zinc-500/10 px-2 py-0.5 text-[10px] text-zinc-500"
            >
              {path}
            </span>
          )}
          <div className="mx-1 h-6 w-px bg-border" />
        </>
      ) : (
        <div className="flex items-center gap-2">
          <button
            onClick={onLoad}
            disabled={loading}
            className="rounded bg-accent px-3 py-1.5 text-[12px] font-semibold text-white hover:bg-cyan-500 disabled:opacity-60"
          >
            {loading ? "Loading model…" : "Load GGUF Model"}
          </button>
          {!loading && lastPath && (
            <span
              title={lastPath}
              className="max-w-72 truncate rounded bg-zinc-500/10 px-2 py-0.5 text-[10px] text-zinc-500"
            >
              last: {fileName(lastPath)}
            </span>
          )}
          <div className="relative">
            <button
              onClick={() => setShowModelSwitcher((v) => !v)}
              aria-label="Browse and download models from the HuggingFace hub"
              className="rounded border border-border px-3 py-1.5 text-[12px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800"
            >
              Browse Models…
            </button>
            {!loading && showModelSwitcher && (
              <div className="absolute left-0 top-full z-30 mt-1 w-80 max-h-72 overflow-auto rounded-md border border-border bg-panel-2 p-2 shadow-xl">
                <div className="mb-1.5 text-[10px] font-semibold uppercase tracking-wide text-zinc-500">
                  Browse models
                </div>
                {downloaded.length > 0 && (
                  <>
                    <div className="mb-1 text-[9px] text-zinc-400">Downloaded</div>
                    {downloaded.map((m) => {
                      const isCurrent = path === m.path;
                      return (
                        <button
                          key={m.path}
                          disabled={isCurrent || isStreaming}
                          onClick={() => { setShowModelSwitcher(false); onSwitchModel(m.path); }}
                          className={`flex w-full items-center gap-2 rounded px-2 py-1 text-left text-[11px] hover:bg-zinc-100 ${isCurrent ? "font-semibold text-accent" : "text-zinc-700"} disabled:opacity-40`}
                          title={m.path}
                        >
                          <span className="truncate flex-1">{m.fileName}</span>
                          <span className="shrink-0 text-[9px] text-zinc-400">{formatBytes(m.sizeBytes)}</span>
                          {isCurrent && <span className="text-[9px] text-accent">active</span>}
                        </button>
                      );
                    })}
                  </>
                )}
                {downloaded.length === 0 && (
                  <p className="px-2 py-2 text-[11px] text-zinc-400">
                    No downloaded models yet — search HuggingFace below.
                  </p>
                )}
                <button
                  onClick={() => { setShowModelSwitcher(false); onLoad(); }}
                  className="mt-2 flex w-full items-center justify-center gap-1 rounded border border-border px-2 py-1 text-[11px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800"
                >
                  + Load from file…
                </button>
                <div className="my-2 border-t border-border" />
                <div className="mb-1 text-[9px] text-zinc-400">Model Hub (HuggingFace)</div>
                <input
                  value={hubQuery}
                  onChange={(e) => setHubQuery(e.target.value)}
                  placeholder="Search GGUF models…"
                  spellCheck={false}
                  className="w-full rounded border border-border bg-panel px-2 py-1 text-[11px] text-ink outline-none focus:border-accent/60"
                />
                {hubBusy && <p className="mt-1 px-0.5 text-[10px] text-zinc-400">Searching hub…</p>}
                {hubError && <p className="mt-1 px-0.5 text-[10px] text-red-500">{hubError}</p>}
                {hubResults && hubResults.length > 0 && (
                  <div className="mt-1 max-h-44 overflow-auto">
                    {hubResults.map((m) => {
                      const gguf = m.files.find((f) => f.name.endsWith(".gguf") && !f.name.includes(".json"));
                      if (!gguf) return null;
                      return (
                        <div key={m.repoId} className="flex flex-col gap-0.5 rounded px-1 py-1 hover:bg-zinc-100">
                          <div className="flex items-center gap-1.5">
                            <span className="min-w-0 flex-1 truncate text-[11px] text-zinc-700" title={m.repoId}>{m.repoId}</span>
                            <span className="shrink-0 text-[9px] text-zinc-400">{m.downloads.toLocaleString()} dl</span>
                          </div>
                          <div className="flex items-center gap-1.5">
                            <span className="min-w-0 flex-1 truncate text-[10px] text-zinc-400">{gguf.name} · {gguf.size != null ? formatBytes(gguf.size) : "?"}</span>
                            <button
                              disabled={isStreaming}
                              onClick={() => startHubDownload(m.repoId, gguf.name)}
                              className="shrink-0 rounded border border-border px-1.5 py-px text-[10px] text-zinc-600 hover:border-accent/50 hover:text-accent disabled:opacity-40"
                            >
                              Download
                            </button>
                          </div>
                          {hubProgress[`${m.repoId}::${gguf.name}`] && (
                            <ProgressBar p={hubProgress[`${m.repoId}::${gguf.name}`]} downloaded={downloaded} onSwitchModel={onSwitchModel} />
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}
                {hubResults && hubResults.length === 0 && !hubBusy && (
                  <p className="mt-1 px-0.5 text-[10px] text-zinc-400">No models found.</p>
                )}
              </div>
            )}
          </div>
          <button
            onClick={() => setShowRemote((v) => !v)}
            aria-label="Connect a remote model provider"
            className="rounded border border-border px-3 py-1.5 text-[12px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800"
          >
            Remote…
          </button>
        </div>
      )}

      {showRemote && !model && (
        <div className="absolute left-3 top-11 z-30 flex w-[28rem] flex-col gap-2 rounded-md border border-border bg-panel-2 p-3 shadow-2xl">
          <div className="flex items-center justify-between">
            <span className="text-[11px] font-semibold uppercase tracking-wide text-zinc-500">
              Connect a model provider
            </span>
            <button
              onClick={() => setShowRemote(false)}
              aria-label="Close"
              title="Close"
              className="rounded p-1 text-zinc-500 hover:bg-panel hover:text-zinc-800"
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
                className="shrink-0 rounded border border-border px-2 py-1 text-[11px] text-zinc-500 hover:border-zinc-400 hover:text-zinc-800 disabled:opacity-40"
              >
                {modelsLoading ? "…" : "↻"}
              </button>
            </div>
            <datalist id="remote-model-options">
              {models.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
            <span className="text-[9px] text-zinc-400">
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
            <p className="max-w-64 text-[10px] leading-snug text-zinc-400">{preset.hint}</p>
            <button
              onClick={submitRemote}
              disabled={!canConnect}
              className="rounded bg-accent px-3 py-1 text-[11px] font-semibold text-white hover:bg-cyan-500 disabled:opacity-40"
            >
              Connect
            </button>
          </div>
          <p className="text-[9px] leading-snug text-zinc-400">
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
          <span className="w-10 text-right text-[11px] tabular-nums text-zinc-500">
            {Math.round(progress * 100)}%
          </span>
        </div>
      )}

      {model && !loading && (
        <div className="flex min-w-0 flex-1 flex-wrap items-center justify-end gap-x-4 gap-y-1 text-[11px] text-zinc-500">
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
          <label className="flex items-center gap-1.5" title="Repetition penalty — raise if the model loops the same output (1 = off)">
            repeat
            <input
              type="number"
              min={1}
              max={2}
              step={0.05}
              value={params.repeatPenalty}
              onChange={(e) => onParamsChange({ repeatPenalty: Number(e.target.value) })}
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
            <summary className="cursor-pointer list-none rounded px-1.5 text-zinc-500 hover:text-zinc-700">
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
              <p className="text-[10px] leading-snug text-zinc-400">
                GPU/context settings take effect on the next model load. CPU-only
                build by default.
              </p>
            </div>
          </details>
          {isStreaming && (
            <button
              onClick={onCancel}
              className="rounded border border-red-400/40 px-2.5 py-1 text-[11px] font-medium text-red-600 hover:bg-red-500/10"
            >
              ■ Stop
            </button>
          )}
        </div>
      )}
    </div>
  );
}
