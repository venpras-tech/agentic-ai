import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";

import { api, isTauriRuntime } from "../lib/ipc";
import type {
  ApiServerStatus,
  DownloadedModel,
  GenParams,
  HfModel,
  HubDownloadProgress,
  McpServerConfig,
} from "../types";

function fmtBytes(n: number | null | undefined): string {
  if (n == null) return "?";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v.toFixed(v >= 100 || u === 0 ? 0 : 1)} ${units[u]}`;
}

interface SettingsModalProps {
  open: boolean;
  onClose: () => void;
  params: GenParams;
  onParamsChange: (patch: Partial<GenParams>) => void;
}

export default function SettingsModal({
  open,
  onClose,
  params,
  onParamsChange,
}: SettingsModalProps) {
  const [local, setLocal] = useState(params);
  const [servers, setServers] = useState<McpServerConfig[] | null>(null);
  const [mcpError, setMcpError] = useState<string | null>(null);
  const [draft, setDraft] = useState({ name: "", bin: "", args: "" });

  // Hub state
  const [hfQuery, setHfQuery] = useState("");
  const [hfResults, setHfResults] = useState<HfModel[] | null>(null);
  const [hfBusy, setHfBusy] = useState(false);
  const [hfError, setHfError] = useState<string | null>(null);
  const [downloaded, setDownloaded] = useState<DownloadedModel[]>([]);
  const [progress, setProgress] = useState<Record<string, HubDownloadProgress>>({});
  const [apiStatus, setApiStatus] = useState<ApiServerStatus | null>(null);
  const [apiPort, setApiPort] = useState(8080);
  const [tab, setTab] = useState<"engine" | "mcp" | "hub" | "server">("engine");

  const TABS = [
    { id: "engine", label: "Engine" },
    { id: "hub", label: "Models" },
    { id: "mcp", label: "MCP" },
    { id: "server", label: "API Server" },
  ] as const;

  const refreshDownloaded = () => {
    api.listDownloadedModels().then(setDownloaded).catch(() => {});
  };

  useEffect(() => {
    if (open) setLocal(params);
  }, [open, params]);

  useEffect(() => {
    if (!open) return;
    setMcpError(null);
    api.mcpCatalogLoad().then(setServers).catch(() => setServers([]));
    refreshDownloaded();
    api.apiServerStatus().then((s) => {
      setApiStatus(s);
      if (s.port) setApiPort(s.port);
    }).catch(() => {});
  }, [open]);

  // Live download progress while the modal is open.
  useEffect(() => {
    if (!open) return;
    const unlisten = listen<HubDownloadProgress>(
      "hf-download-progress",
      (e) => {
        const p = e.payload;
        setProgress((prev) => ({ ...prev, [`${p.repoId}::${p.file}`]: p }));
        if (p.done) refreshDownloaded();
      },
    );
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  const apply = () => {
    onParamsChange(local);
    onClose();
  };

  const saveServers = async (next: McpServerConfig[]) => {
    setServers(next);
    setMcpError(null);
    try {
      await api.mcpCatalogSave(next);
    } catch (e) {
      setMcpError(String(e));
    }
  };

  const addServer = () => {
    if (!servers) return;
    const name = draft.name.trim();
    const bin = draft.bin.trim();
    if (!name || !bin) return;
    const args = draft.args.split(/\s+/).filter(Boolean);
    void saveServers([...servers, { name, bin, args, enabled: true }]);
    setDraft({ name: "", bin: "", args: "" });
  };

  const searchHub = async () => {
    if (!hfQuery.trim()) return;
    setHfBusy(true);
    setHfError(null);
    try {
      setHfResults(await api.hfSearch(hfQuery.trim(), 12));
    } catch (e) {
      setHfError(String(e));
    } finally {
      setHfBusy(false);
    }
  };

  const startDownload = (repoId: string, file: string) => {
    api.hfDownloadModel(repoId, file).catch((e) => setHfError(String(e)));
  };

  const loadLocal = async (path: string) => {
    try {
      await api.loadModelFromPath(path);
      onClose();
    } catch (e) {
      setHfError(String(e));
    }
  };

  const toggleApiServer = async () => {
    try {
      const next = apiStatus?.running
        ? await api.apiServerStop()
        : await api.apiServerStart(apiPort);
      setApiStatus(next);
    } catch (e) {
      setApiStatus({ running: false, port: null });
      setMcpError(String(e));
    }
  };

  return (
    <div
      className="fixed inset-0 z-40 flex items-center justify-center bg-black/30"
      role="dialog"
      aria-modal="true"
      aria-label="Settings"
    >
      <div className="flex max-h-[85vh] w-[32rem] max-w-[94vw] flex-col gap-4 rounded-lg border border-border bg-panel-2 p-5 shadow-2xl">
        <div className="flex items-center justify-between">
          <span className="text-[13px] font-semibold text-ink">Settings</span>
          <button
            onClick={onClose}
            className="rounded p-1 text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M18 6 6 18M6 6l12 12" />
            </svg>
          </button>
        </div>

        <div className="flex gap-1 border-b border-border pb-2">
          {TABS.map((t) => (
            <button
              key={t.id}
              onClick={() => setTab(t.id)}
              className={`rounded px-2.5 py-1 text-[11px] font-semibold transition-colors ${
                tab === t.id
                  ? "bg-accent/15 text-cyan-600"
                  : "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
              }`}
            >
              {t.label}
            </button>
          ))}
        </div>

        <div className="min-h-0 flex-1 overflow-y-auto">
        {tab === "engine" && (
        <>

        <div className="grid grid-cols-2 gap-3">
          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Context Size
            <select
              value={local.contextSize}
              onChange={(e) => setLocal((p) => ({ ...p, contextSize: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            >
              {[2048, 4096, 8192, 16384, 32768].map((n) => (
                <option key={n} value={n}>{n}</option>
              ))}
            </select>
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Threads
            <input
              type="number"
              min={1}
              max={256}
              value={local.nThreads}
              onChange={(e) => setLocal((p) => ({ ...p, nThreads: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            GPU Layers
            <input
              type="number"
              min={0}
              max={999}
              value={local.nGpuLayers}
              onChange={(e) => setLocal((p) => ({ ...p, nGpuLayers: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Max Tokens
            <input
              type="number"
              min={16}
              max={16384}
              step={16}
              value={local.maxTokens}
              onChange={(e) => setLocal((p) => ({ ...p, maxTokens: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Temperature
            <input
              type="number"
              min={0}
              max={2}
              step={0.05}
              value={local.temperature}
              onChange={(e) => setLocal((p) => ({ ...p, temperature: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500">
            Top P
            <input
              type="number"
              min={0}
              max={1}
              step={0.05}
              value={local.topP}
              onChange={(e) => setLocal((p) => ({ ...p, topP: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] text-zinc-500" title="Suppresses repeated tokens — raise if the model loops the same output. 1 = off.">
            Repeat penalty
            <input
              type="number"
              min={1}
              max={2}
              step={0.05}
              value={local.repeatPenalty}
              onChange={(e) => setLocal((p) => ({ ...p, repeatPenalty: Number(e.target.value) }))}
              className="rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent/60"
            />
          </label>
        </div>

        <p className="text-[9px] text-zinc-400">
          GPU and context settings take effect on the next model load. Temperature, top-p and
          repeat penalty apply to every generation.
        </p>
        </>
        )}

        {tab === "mcp" && (
        <>

        <div className="flex flex-col gap-2 border-t border-border pt-3">
          <span className="text-[11px] font-semibold text-ink">
            MCP Servers{" "}
            <span className="font-normal text-zinc-400">
              (stdio; callable by the agent via call_mcp_tool)
            </span>
          </span>
          {mcpError && (
            <p className="rounded border border-red-400/40 bg-red-500/10 px-2 py-1 text-[10px] text-red-600">
              {mcpError}
            </p>
          )}
          {servers && servers.length > 0 && (
            <div className="flex flex-col gap-1">
              {servers.map((s) => (
                <div
                  key={s.name}
                  className="flex items-center gap-2 rounded border border-border bg-panel px-2 py-1"
                >
                  <input
                    type="checkbox"
                    checked={s.enabled}
                    onChange={(e) =>
                      void saveServers(
                        servers.map((x) =>
                          x.name === s.name ? { ...x, enabled: e.target.checked } : x,
                        ),
                      )
                    }
                    title="Enabled servers are callable by name"
                    className="accent-cyan-400"
                  />
                  <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-ink">
                    {s.name}
                    <span className="ml-2 text-zinc-500">{s.bin} {s.args.join(" ")}</span>
                    {!!s.allowedTools?.length && (
                      <span
                        className="ml-2 rounded bg-panel-2 px-1 py-px text-[9px] text-amber-600"
                        title={`Allowed tools: ${s.allowedTools.join(", ")} (trailing * = prefix wildcard)`}
                      >
                        ⚑ {s.allowedTools.length}
                      </span>
                    )}
                  </span>
                  <button
                    onClick={() => void saveServers(servers.filter((x) => x.name !== s.name))}
                    aria-label={`Remove ${s.name}`}
                    className="rounded p-0.5 text-zinc-400 hover:bg-red-500/10 hover:text-red-500"
                  >
                    <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6" strokeLinecap="round">
                      <path d="M18 6 6 18M6 6l12 12" />
                    </svg>
                  </button>
                </div>
              ))}
            </div>
          )}
          {servers && servers.length === 0 && (
            <p className="text-[10px] text-zinc-500">No MCP servers configured.</p>
          )}
          <div className="flex items-center gap-1.5">
            <input
              value={draft.name}
              onChange={(e) => setDraft((d) => ({ ...d, name: e.target.value }))}
              placeholder="name"
              className="w-20 rounded border border-border bg-panel px-2 py-1 font-mono text-[10.5px] text-ink outline-none focus:border-accent/60"
            />
            <input
              value={draft.bin}
              onChange={(e) => setDraft((d) => ({ ...d, bin: e.target.value }))}
              placeholder="npx @playwright/mcp@latest"
              className="min-w-0 flex-1 rounded border border-border bg-panel px-2 py-1 font-mono text-[10.5px] text-ink outline-none focus:border-accent/60"
            />
            <input
              value={draft.args}
              onChange={(e) => setDraft((d) => ({ ...d, args: e.target.value }))}
              placeholder="--args…"
              className="w-24 rounded border border-border bg-panel px-2 py-1 font-mono text-[10.5px] text-ink outline-none focus:border-accent/60"
            />
            <button
              onClick={addServer}
              disabled={!draft.name.trim() || !draft.bin.trim()}
              className="rounded bg-accent/15 px-2 py-1 text-[10.5px] font-semibold text-cyan-600 hover:bg-accent/25 disabled:opacity-40"
            >
              + Add
            </button>
          </div>
        </div>
        </>
        )}

        {tab === "hub" && (
        <>

        <div className="flex flex-col gap-2 border-t border-border pt-3">
          <span className="text-[11px] font-semibold text-ink">
            Model Hub{" "}
            <span className="font-normal text-zinc-400">(Hugging Face GGUF)</span>
          </span>
          {hfError && (
            <p className="rounded border border-red-400/40 bg-red-500/10 px-2 py-1 text-[10px] text-red-600">
              {hfError}
            </p>
          )}
          {downloaded.length > 0 && (
            <div className="flex flex-col gap-1">
              <span className="text-[10px] text-zinc-500">Downloaded</span>
              {downloaded.map((d) => (
                <div
                  key={d.path}
                  className="flex items-center gap-2 rounded border border-border bg-panel px-2 py-1"
                >
                  <span className="min-w-0 flex-1 truncate font-mono text-[10.5px] text-ink">
                    {d.repoId}/{d.fileName}
                    <span className="ml-2 text-zinc-500">{fmtBytes(d.sizeBytes)}</span>
                  </span>
                  <button
                    onClick={() => void loadLocal(d.path)}
                    title={`Load ${d.fileName}`}
                    className="rounded bg-emerald-500/15 px-2 py-0.5 text-[10px] font-semibold text-emerald-600 hover:bg-emerald-500/25"
                  >
                    Load
                  </button>
                </div>
              ))}
            </div>
          )}
          <div className="flex items-center gap-1.5">
            <input
              value={hfQuery}
              onChange={(e) => setHfQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void searchHub();
              }}
              placeholder="Search GGUF models (e.g. qwen2.5 coder)"
              className="min-w-0 flex-1 rounded border border-border bg-panel px-2 py-1 text-[11px] text-ink outline-none focus:border-accent/60"
            />
            <button
              onClick={() => void searchHub()}
              disabled={hfBusy || isTauriRuntime() === false}
              className="rounded bg-accent/15 px-2 py-1 text-[10.5px] font-semibold text-cyan-600 hover:bg-accent/25 disabled:opacity-40"
            >
              {hfBusy ? "…" : "Search"}
            </button>
          </div>
          {hfResults && (
            <div className="flex max-h-44 flex-col gap-1 overflow-y-auto">
              {hfResults.length === 0 && (
                <p className="text-[10px] text-zinc-500">No GGUF repos matched.</p>
              )}
              {hfResults.map((m) =>
                m.files.slice(0, 4).map((f) => {
                  const key = `${m.repoId}::${f.name}`;
                  const p = progress[key];
                  const pct =
                    p?.totalBytes && p.receivedBytes != null
                      ? Math.min(100, Math.round((p.receivedBytes / p.totalBytes) * 100))
                      : null;
                  return (
                    <div
                      key={key}
                      className="flex items-center gap-2 rounded border border-border bg-panel px-2 py-1"
                    >
                      <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-zinc-700">
                        {m.repoId.split("/")[1] ?? m.repoId}
                        <span className="ml-1.5 text-zinc-400">{f.name}</span>
                      </span>
                      <span className="shrink-0 text-[9px] text-zinc-400">
                        ↓{m.downloads.toLocaleString()}
                      </span>
                      {pct != null && !p.done ? (
                        <>
                          <span className="w-16 shrink-0 rounded bg-panel-2 text-center font-mono text-[9.5px] text-cyan-600">
                            {pct}%
                          </span>
                          <button
                            onClick={() => void api.hfCancelDownload(m.repoId, f.name)}
                            className="shrink-0 rounded bg-red-500/10 px-1.5 py-0.5 text-[9.5px] text-red-500"
                          >
                            ✕
                          </button>
                        </>
                      ) : (
                        <button
                          onClick={() => startDownload(m.repoId, f.name)}
                          className="shrink-0 rounded bg-accent/15 px-2 py-0.5 text-[9.5px] font-semibold text-cyan-600 hover:bg-accent/25"
                        >
                          {p?.done ? "✓" : fmtBytes(f.size)}
                        </button>
                      )}
                    </div>
                  );
                }),
              )}
            </div>
          )}
        </div>
        </>
        )}

        {tab === "server" && (
        <>
        <div className="flex flex-col gap-2 border-t border-border pt-3">
          <span className="text-[11px] font-semibold text-ink">
            Local API Server{" "}
            <span className="font-normal text-zinc-400">
              (OpenAI-compatible, 127.0.0.1 only)
            </span>
          </span>
          <div className="flex items-center gap-2">
            <input
              type="number"
              min={1024}
              max={65535}
              value={apiPort}
              onChange={(e) => setApiPort(Number(e.target.value))}
              disabled={apiStatus?.running ?? false}
              className="w-24 rounded border border-border bg-panel px-2 py-1 font-mono text-[10.5px] text-ink outline-none focus:border-accent/60 disabled:opacity-50"
            />
            <button
              onClick={() => void toggleApiServer()}
              className={`rounded px-3 py-1 text-[10.5px] font-semibold ${
                apiStatus?.running
                  ? "bg-red-500/15 text-red-600 hover:bg-red-500/25"
                  : "bg-emerald-500/15 text-emerald-600 hover:bg-emerald-500/25"
              }`}
            >
              {apiStatus?.running ? `■ Stop :${apiStatus.port}` : "▶ Start"}
            </button>
            {apiStatus?.running && (
              <span className="font-mono text-[9.5px] text-zinc-500">
                POST /v1/chat/completions
              </span>
            )}
          </div>
        </div>
        </>
        )}
        </div>

        <div className="flex justify-end gap-2">
          <button
            onClick={onClose}
            className="rounded border border-border px-3 py-1.5 text-[11px] text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
          >
            {tab === "engine" ? "Cancel" : "Close"}
          </button>
          {tab === "engine" && (
            <button
              onClick={apply}
              className="rounded bg-accent px-3 py-1.5 text-[11px] font-semibold text-white hover:bg-cyan-500"
            >
              Apply
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
