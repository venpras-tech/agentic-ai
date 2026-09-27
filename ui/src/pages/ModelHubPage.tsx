import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../lib/ipc";
import CancelDownloadDialog from "../components/CancelDownloadDialog";
import { useDownloadManager, formatBytes, formatSpeed, formatEta, parseQuant, estimateRamGb, fitStatus, dlKey, GB, type DownloadProgress } from "../lib/downloads";
import "./ModelHubPage.css";

interface HfResult {
  repoId: string;
  author: string | null;
  likes: number;
  downloads: number;
  files: Array<{ name: string; size: number | null }>;
}

interface SystemInfo {
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

type SortKey = "relevance" | "downloads" | "likes" | "name" | "size";

/** How often the hardware figures are re-read. */
const SYSTEM_POLL_MS = 3000;

/**
 * Headroom thresholds for a resource, as a fraction of capacity.
 *
 * The bands are deliberately generous: the point is to warn before a download
 * or a model load is starved, not to track load average precisely.
 */
const STATUS_BANDS = {
  /** Free fraction: below `red` is critical, below `yellow` is tight. */
  yellow: 0.15,
  red: 0.05,
} as const;

type StatusLevel = "ok" | "warn" | "crit";

export function levelFor(freeGb: number, totalGb: number): StatusLevel {
  if (!Number.isFinite(freeGb) || !Number.isFinite(totalGb) || totalGb <= 0) return "warn";
  const free = Math.max(0, Math.min(1, freeGb / totalGb));
  if (free <= STATUS_BANDS.red) return "crit";
  if (free <= STATUS_BANDS.yellow) return "warn";
  return "ok";
}

/** Total RAM is a capacity fact: only flag it when the machine is tiny. */
export function ramCapacityLevel(totalGb: number): StatusLevel {
  if (totalGb < 4) return "crit";
  if (totalGb < 8) return "warn";
  return "ok";
}

const STATUS_TEXT: Record<StatusLevel, string> = {
  ok: "Healthy",
  warn: "Low",
  crit: "Critical",
};

/** A single monitor row: coloured dot, label, value, and a status hint. */
function SysItem({
  level,
  label,
  value,
  title,
}: {
  level: StatusLevel | null;
  label: string;
  value: string;
  title?: string;
}) {
  return (
    <div className="sys-item" title={title}>
      <span className="sys-item-head">
        <span className={`sys-dot sys-dot-${level ?? "none"}`} aria-hidden="true" />
        <span className="sys-label">{label}</span>
      </span>
      <span className="sys-val">{value}</span>
      {level && <span className={`sys-state sys-state-${level}`}>{STATUS_TEXT[level]}</span>}
    </div>
  );
}

const SORT_LABELS: Record<SortKey, string> = {
  relevance: "Relevance",
  downloads: "Downloads",
  likes: "Likes",
  name: "Name",
  size: "Total size",
};

function compactNum(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return String(n);
}

function fitBadge(status: "fits" | "tight" | "no" | "unknown") {
  if (status === "fits") return <span className="fit-badge fits">Fits comfortably</span>;
  if (status === "tight") return <span className="fit-badge tight">Tight fit</span>;
  if (status === "no") return <span className="fit-badge no">Insufficient RAM</span>;
  return <span className="fit-badge unknown">Unknown</span>;
}

/** Only GGUF weight files count for size/fit math and download rows. */
function modelFiles(files: Array<{ name: string; size: number | null }>) {
  const weights = files.filter((f) => !/imatrix/i.test(f.name));
  const quant = weights.filter((f) => parseQuant(f.name) !== "—");
  return quant.length > 0 ? quant : weights;
}

function fileSizeRange(files: Array<{ name: string; size: number | null }>): { min: number; max: number } {
  const sizes = modelFiles(files).map((f) => f.size || 0).filter((s) => s > 0);
  if (sizes.length === 0) return { min: 0, max: 0 };
  return { min: Math.min(...sizes), max: Math.max(...sizes) };
}

/**
 * Compatible = the smallest GGUF quant fits your RAM. Uses the SAME estimate
 * and threshold as the table's "Your Fit" column (recommended 8k context), so
 * the dot and the rows can never disagree.
 */
function compatLevel(
  availableGb: number,
  files: Array<{ name: string; size: number | null }>,
): "ok" | "bad" | "unknown" {
  if (!availableGb) return "unknown";
  const { min } = fileSizeRange(files);
  if (min <= 0) return "unknown";
  const ram = estimateRamGb(min, 8192);
  const fit = fitStatus(availableGb, ram);
  return fit === "no" ? "bad" : fit === "unknown" ? "unknown" : "ok";
}

function compatDot(level: "ok" | "bad" | "unknown") {
  const title =
    level === "ok"
      ? "Compatible: the smallest quant fits in your available RAM."
      : level === "bad"
        ? "Not compatible: even the smallest quant exceeds your available RAM."
        : "Compatibility unknown (system info unavailable).";
  return <span className={`hub-dot ${level}`} title={title} aria-label={title} />;
}

function progressCell(d: DownloadProgress | undefined, repoId: string, file: string) {
  if (!d) return null;
  const pct = d.totalBytes > 0 ? Math.round((d.receivedBytes / d.totalBytes) * 100) : 0;
  const speed = formatSpeed(d.speedBps);
  const eta = formatEta(d.etaSeconds);
  return (
    <div className="table-progress">
      {d.done ? (
        <span className="dl-status done">Complete</span>
      ) : d.cancelled ? (
        <span className="dl-status">Cancelled</span>
      ) : d.paused ? (
        <span className="dl-status paused">Paused</span>
      ) : d.error ? (
        <span className="dl-status error" title={d.error}>Error</span>
      ) : (
        <>
          <span className="dl-pct">{pct}%</span>
          <div className="table-progress-track">
            <div className="table-progress-fill" style={{ width: `${pct}%` }} />
          </div>
          {speed && <span className="dl-rate">{speed}{eta ? ` · ${eta}` : ""}</span>}
        </>
      )}
    </div>
  );
}

function ModelHubPage() {
  const navigate = useNavigate();
  const { downloads, download, pause, resume, cancel } = useDownloadManager();
  const [query, setQuery] = useState(() => localStorage.getItem("hf-last-query") || "llama gguf");
  const [sortBy, setSortBy] = useState<SortKey>(() => {
    const stored = localStorage.getItem("hf-sort") as SortKey | null;
    return stored && stored in SORT_LABELS ? stored : "relevance";
  });
  const [results, setResults] = useState<HfResult[]>([]);
  const [openRepo, setOpenRepo] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState("");
  const [sys, setSys] = useState<SystemInfo | null>(null);
  const [downloaded, setDownloaded] = useState<Record<string, boolean>>({});
  const [note, setNote] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<{ repoId: string; file: string } | null>(null);

  const loadDownloaded = useCallback(async () => {
    try {
      const list = await api.listDownloadedModels();
      const map: Record<string, boolean> = {};
      for (const m of list) map[dlKey(m.repoId, m.fileName)] = true;
      setDownloaded(map);
    } catch { setDownloaded({}); }
  }, []);

  useEffect(() => {
    void loadDownloaded();
  }, [loadDownloaded]);

  // Live hardware monitor. RAM and disk move while a download is running, so
  // the figures are re-polled instead of frozen at mount. The poll is skipped
  // while the tab is hidden to avoid pointless work in the background.
  useEffect(() => {
    let timer: number | undefined;
    const tick = () => {
      if (document.visibilityState === "visible") {
        void api.systemInfo().then(setSys).catch(() => {});
      }
    };
    tick();
    timer = window.setInterval(tick, SYSTEM_POLL_MS);
    return () => {
      if (timer !== undefined) window.clearInterval(timer);
    };
  }, []);

  // Whenever any download completes, refresh the set of files already on disk
  // so the row flips to "On disk".
  useEffect(() => {
    const doneKeys = Object.values(downloads).filter((d) => d.done).map((d) => dlKey(d.repoId, d.file));
    if (doneKeys.length > 0) void loadDownloaded();
  }, [downloads, loadDownloaded]);

  const persistQuery = (q: string) => {
    setQuery(q);
    localStorage.setItem("hf-last-query", q);
  };

  const handleSearch = async () => {
    setSearching(true);
    setError("");
    setResults([]);
    localStorage.setItem("hf-last-query", query.trim() || "llama gguf");
    try {
      const res = await api.hfSearch(query.trim() || "llama gguf", 20);
      setResults(res);
      setOpenRepo(res.length > 0 ? res[0].repoId : null);
      if (res.length === 0) setError("No GGUF models found for that query.");
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
    }
    setSearching(false);
  };

  const handleSort = (k: SortKey) => {
    setSortBy(k);
    localStorage.setItem("hf-sort", k);
  };

  const sortedResults = useMemo(() => {
    const available = sys?.availableRamGb || 0;
    const arr = [...results];
    const compatRank = (files: HfResult["files"]) =>
      compatLevel(available, files) === "ok" ? 0 : compatLevel(available, files) === "bad" ? 1 : 2;
    const byCompat = (a: HfResult, b: HfResult) => compatRank(a.files) - compatRank(b.files);
    switch (sortBy) {
      case "downloads":
        arr.sort((a, b) => b.downloads - a.downloads);
        break;
      case "likes":
        arr.sort((a, b) => b.likes - a.likes);
        break;
      case "name":
        arr.sort((a, b) => a.repoId.localeCompare(b.repoId));
        break;
      case "size":
        arr.sort((a, b) => {
          const sa = a.files.reduce((acc, f) => acc + (f.size || 0), 0);
          const sb = b.files.reduce((acc, f) => acc + (f.size || 0), 0);
          return sb - sa;
        });
        break;
      default:
        break;
    }
    arr.sort((a, b) => byCompat(a, b) || 0);
    return arr;
  }, [results, sortBy, sys]);

  const diskFreeBytes = (sys?.diskFreeGb || 0) * GB;

  const handleDownloadAction = async (repoId: string, file: string) => {
    const key = dlKey(repoId, file);
    const d = downloads[key];
    if (d?.done || downloaded[key]) return;
    if (d?.paused) { void resume(repoId, file); return; }
    if (d && !d.cancelled && !d.error) { void pause(repoId, file); return; }
    try {
      await download(repoId, file);
      setNote(`Downloading ${file.split("/").pop()}…`);
    } catch (err) {
      setError(`Download failed to start: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const actionLabel = (repoId: string, file: string): string => {
    const key = dlKey(repoId, file);
    const d = downloads[key];
    if (downloaded[key] && !d) return "On disk";
    if (d?.done) return "On disk";
    if (d?.paused) return "Resume";
    if (d && !d.cancelled && !d.error) return "Pause";
    return "Download";
  };

  const actionClass = (repoId: string, file: string): string => {
    const key = dlKey(repoId, file);
    const d = downloads[key];
    if (downloaded[key] && !d) return "hub-dl-btn on-disk";
    if (d?.done) return "hub-dl-btn on-disk";
    if (d?.paused) return "hub-dl-btn resume";
    if (d && !d.cancelled && !d.error) return "hub-dl-btn active";
    return "hub-dl-btn";
  };

  const handleDownloadAll = async (model: HfResult) => {
    const diskFree = diskFreeBytes;
    let started = 0;
    for (const f of modelFiles(model.files)) {
      const key = dlKey(model.repoId, f.name);
      if (!downloaded[key] && !downloads[key] && (f.size ?? 0) <= diskFree) {
        try {
          await download(model.repoId, f.name);
          started += 1;
          setNote(`Starting download of all files that fit on your disk.`);
        } catch (err) {
          setError(`Download failed to start: ${err instanceof Error ? err.message : String(err)}`);
        }
      }
    }
    if (started === 0 && !error) setNote(`No files to download — all already on disk (or don't fit).`);
  };

  const openOnHF = (repoId: string) => {
    if (note) setNote(`${repoId} – opening model page in your browser…`);
    void api.openUrl(`https://huggingface.co/${repoId}`).catch((err) =>
      setNote(`Couldn't open browser: ${String(err)}`),
    );
  };

  return (
    <div className="hub-view">
      <div className="hub-header">
        <h2 className="hub-title">Hugging Face Hub</h2>
        <p className="hub-desc">
          Search and download GGUF models. Downloaded models appear in the
          <button type="button" className="hub-link" onClick={() => navigate("/models")}>Models</button>
          page for loading. Your system spec is analyzed to suggest whether a model will fit.
        </p>
      </div>

      {note && (
        <div className="hub-note" role="status">
          {note}
          <button type="button" className="hub-note-close" onClick={() => setNote(null)}>×</button>
        </div>
      )}

      {sys && (
        <div className="sys-card">
          <div className="sys-card-title">
            Your System
            <span className="sys-live" title="Refreshes every few seconds">
              <span className="sys-live-dot" aria-hidden="true" />
              live
            </span>
          </div>
          <div className="sys-grid">
            <SysItem level={null} label="OS" value={sys.os} />
            <SysItem level={null} label="CPU" value={sys.cpu} />
            <SysItem level={null} label="Cores" value={String(sys.cores)} />
            <SysItem
              level={ramCapacityLevel(sys.totalRamGb)}
              label="Total RAM"
              value={`${sys.totalRamGb.toFixed(1)} GB`}
              title="Total installed memory. Yellow under 8 GB, red under 4 GB."
            />
            <SysItem
              level={levelFor(sys.availableRamGb, sys.totalRamGb)}
              label="Available RAM"
              value={`${sys.availableRamGb.toFixed(1)} GB`}
              title={`${((sys.availableRamGb / Math.max(sys.totalRamGb, 1)) * 100).toFixed(0)}% of ${sys.totalRamGb.toFixed(1)} GB free`}
            />
            <SysItem
              level={levelFor(sys.diskFreeGb, sys.diskTotalGb)}
              label="Disk Free"
              value={`${sys.diskFreeGb.toFixed(1)} GB / ${sys.diskTotalGb.toFixed(1)} GB`}
              title={`${((sys.diskFreeGb / Math.max(sys.diskTotalGb, 1)) * 100).toFixed(0)}% free on the models volume`}
            />
            {sys.gpu ? (
              <SysItem
                level={null}
                label="GPU"
                value={`${sys.gpu.name} (${sys.gpu.vramGb.toFixed(1)} GB)`}
                title="GPU and VRAM reported by nvidia-smi"
              />
            ) : (
              <SysItem
                level={sys.gpuEnabled ? "ok" : "warn"}
                label="GPU"
                value={`None detected${sys.gpuEnabled ? "" : " (CPU-only build)"}`}
                title={
                  sys.gpuEnabled
                    ? "No GPU reported. The build has GPU support enabled, so check the driver."
                    : "This binary was compiled without a llama.cpp GPU backend."
                }
              />
            )}
          </div>
        </div>
      )}

      <div className="hub-search-bar">
        <div className="hub-search-input-wrap">
          <svg className="hub-search-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="11" cy="11" r="8" />
            <path d="m21 21-4.3-4.3" />
          </svg>
          <input
            type="text"
            className="hub-search-input"
            placeholder="Search GGUF models… e.g. llama, qwen, phi"
            value={query}
            onChange={(e) => persistQuery(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Enter") void handleSearch(); }}
          />
        </div>
        <button type="button" className="hub-search-btn" onClick={() => void handleSearch()} disabled={searching}>
          {searching ? "Searching…" : "Search"}
        </button>
      </div>

      {error ? <p className="hub-error">{error}</p> : null}
      {searching ? <div className="hub-spinner-wrap"><span className="spinner" /></div> : null}

      {!searching && results.length > 0 && (
        <div className="hub-sort-row">
          <span className="hub-sort-label">Sort by</span>
          {Object.entries(SORT_LABELS).map(([k, label]) => (
            <button
              key={k}
              type="button"
              className={`hub-sort-btn ${sortBy === k ? "active" : ""}`}
              onClick={() => handleSort(k as SortKey)}
            >
              {label}
            </button>
          ))}
          <span className="hub-sort-hint">Compatible models are always listed first</span>
        </div>
      )}

      <div className="hub-results">
        {sortedResults.map((model) => {
          const allSizes = model.files.reduce((acc, f) => acc + (f.size || 0), 0);
          const { min, max } = fileSizeRange(model.files);
          const level = compatLevel(sys?.availableRamGb || 0, model.files);
          const isOpen = openRepo === model.repoId;
          return (
            <div key={model.repoId} className="hub-acc-item">
              <button
                type="button"
                className="hub-acc-head"
                aria-expanded={isOpen}
                onClick={() => setOpenRepo(isOpen ? null : model.repoId)}
              >
                <span className="hub-acc-title-wrap">
                  <span className="hub-result-title" title={model.repoId}>{model.repoId}</span>
                  {model.author && <span className="hub-author">{model.author}</span>}
                </span>
                <span className="hub-acc-meta">
                  <span className="hub-acc-size" title="Smallest available quant file">Min {formatBytes(min)}</span>
                  <span className="hub-acc-size" title="Largest available quant file">Max {formatBytes(max)}</span>
                  {compatDot(level)}
                  <span className="hub-acc-chevron" aria-hidden="true">{isOpen ? "▾" : "▸"}</span>
                </span>
              </button>

              {isOpen && (
                <div className="hub-acc-body">
                  <div className="hub-result-head">
                    <div className="hub-result-title-wrap">
                      <div className="hub-result-sub">
                        <button type="button" className="hub-open-btn" onClick={() => openOnHF(model.repoId)} title="Open on huggingface.co">
                          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                            <path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6" />
                            <path d="M15 3h6v6" />
                            <path d="M10 14 21 3" />
                          </svg>
                          Open model page
                        </button>
                      </div>
                    </div>
                    <div className="hub-result-metrics">
                      <span className="hub-metric" title="Downloads">⬇ {compactNum(model.downloads)}</span>
                      <span className="hub-metric" title="Likes">★ {compactNum(model.likes)}</span>
                      {allSizes > 0 && <span className="hub-metric hub-size-total">Σ {formatBytes(allSizes)}</span>}
                      {model.files.length > 1 && (
                        <button type="button" className="hub-dl-all-btn" onClick={() => void handleDownloadAll(model)}>
                          Download all
                        </button>
                      )}
                    </div>
                  </div>

                  <div className="hub-table-wrap">
                    <table className="hub-table">
                      <thead>
                        <tr>
                          <th>File</th>
                          <th>Size</th>
                          <th>Quant</th>
                          <th className="ram-col" title="Estimated RAM for a typical 8k-token context window: model size + ~12 % overhead + ~1.5 GB context. Change n_ctx in Settings to match your usage.">
                            Recommended RAM
                          </th>
                          <th className="ram-col" title="Smallest usable footprint: model file size + ~12 % overhead + ~0.8 GB for a short 4k-token context. Real minimum will still depend on Settings → n_ctx.">
                            Minimum RAM
                          </th>
                          <th>Your Fit</th>
                          <th>Progress</th>
                          <th />
                        </tr>
                      </thead>
                      <tbody>
                        {[...modelFiles(model.files)]
                          .sort((a, b) => (a.size || 0) - (b.size || 0))
                          .map((f) => {
                          const size = f.size || 0;
                          const est = estimateRamGb(size, 8192);
                          const min = estimateRamGb(size, 4096);
                          const fit = fitStatus(sys?.availableRamGb || 0, est);
                          const noDisk = size > diskFreeBytes;
                          const key = dlKey(model.repoId, f.name);
                          const dl = downloads[key];
                          const done = downloaded[key] && !dl;
                          const label = actionLabel(model.repoId, f.name);
                          const cls = actionClass(model.repoId, f.name);
                          const disabled = !done && noDisk && !dl;
                          return (
                            <tr key={f.name}>
                              <td className="hub-table-fname" title={f.name}>{f.name}</td>
                              <td className="hub-table-num">
                                {formatBytes(size)}
                                {noDisk && (
                                  <div className="hub-warn" title={`This file needs ${formatBytes(size)} but only ${formatBytes(diskFreeBytes)} is free.`}>
                                    Not enough disk
                                  </div>
                                )}
                              </td>
                              <td className="hub-table-num">{parseQuant(f.name)}</td>
                              <td className="hub-table-num">{est > 0 ? `${est.toFixed(1)} GB` : "—"}</td>
                              <td className="hub-table-num">{min > 0 ? `${min.toFixed(1)} GB` : "—"}</td>
                              <td>{fitBadge(fit)}</td>
                              <td>{progressCell(dl, model.repoId, f.name)}</td>
                              <td className="hub-table-action">
                                {done ? (
                                  <span className="hub-dl-btn on-disk">On disk</span>
                                ) : dl?.error ? (
                                  <button type="button" className="hub-dl-btn error" onClick={() => void handleDownloadAction(model.repoId, f.name)} title={dl.error}>Retry</button>
                                ) : (
                                  <button
                                    type="button"
                                    className={cls}
                                    disabled={disabled}
                                    title={disabled ? `Not enough free disk space (needs ${formatBytes(size)})` : undefined}
                                    onClick={() => void handleDownloadAction(model.repoId, f.name)}
                                  >
                                    {label}
                                  </button>
                                )}
                                {!done && dl && !dl.done && (dl.receivedBytes > 0 || dl.totalBytes > 0) ? (
                                  <button type="button" className="hub-dl-cancel" onClick={() => setConfirming({ repoId: model.repoId, file: f.name })} title="Cancel">
                                    <svg width="12" height="12" viewBox="0 0 12 12"><line x1="2" y1="2" x2="10" y2="10" stroke="currentColor" strokeWidth="1.5"/><line x1="10" y1="2" x2="2" y2="10" stroke="currentColor" strokeWidth="1.5"/></svg>
                                  </button>
                                ) : null}
                              </td>
                            </tr>
                          );
                        })}
                      </tbody>
                    </table>
                  </div>
                </div>
              )}
            </div>
          );
        })}
      </div>

      {!searching && results.length > 0 && (
        <div className="hub-footer-hint">
          Public GGUF repos download directly — no token needed. Gated models require you to accept the license on huggingface.co first. RAM figures are estimates; the fit badge compares your available RAM against the recommended (8k context) figure.
        </div>
      )}

      <CancelDownloadDialog
        open={confirming !== null}
        repoId={confirming?.repoId ?? ""}
        file={confirming?.file ?? ""}
        onCancel={(r, f, del) => cancel(r, f, del)}
        onClose={() => setConfirming(null)}
      />
    </div>
  );
}

export default ModelHubPage;