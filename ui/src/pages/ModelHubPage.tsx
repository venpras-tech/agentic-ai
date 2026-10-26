import { useCallback, useEffect, useMemo, useState } from "react";
import { useNavigate } from "react-router-dom";
import { api } from "../lib/ipc";
import CancelDownloadDialog from "../components/CancelDownloadDialog";
import {
  useDownloadManager,
  formatBytes,
  formatSpeed,
  formatEta,
  parseQuant,
  estimateRamGb,
  dlKey,
  GB,
  type DownloadProgress,
} from "../lib/downloads";
import {
  type HfResult,
  type SystemInfo,
  type SortKey,
  type StatusLevel,
  levelFor,
  ramCapacityLevel,
  SORT_LABELS,
  POPULAR_FAMILIES,
  compactNum,
  fitForModel,
  downloadUnits,
  recommendedUnit,
  isGatedModel,
  isInstructModel,
  fileSizeRange,
  compatLevel,
  parseRepoDisplay,
  normalizeHubSearchQuery,
} from "../lib/modelHubUtils";
import "./AgentsPage.css";
import "./ModelsPage.css";
import "./ModelHubPage.css";

export { levelFor, ramCapacityLevel };

const SYSTEM_POLL_MS = 3000;
const PAGE_SIZE = 40;

const STATUS_TEXT: Record<StatusLevel, string> = {
  ok: "Healthy",
  warn: "Low",
  crit: "Critical",
};

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
    <div className="hub-sys-item" title={title}>
      <span className="hub-sys-head">
        <span className={`hub-sys-dot hub-sys-dot-${level ?? "none"}`} aria-hidden="true" />
        <span className="hub-sys-label">{label}</span>
      </span>
      <span className="hub-sys-val">{value}</span>
      {level ? <span className={`hub-sys-state hub-sys-state-${level}`}>{STATUS_TEXT[level]}</span> : null}
    </div>
  );
}

function fitBadge(status: "fits" | "tight" | "no" | "unknown") {
  if (status === "fits") return <span className="hub-fit fits">Fits</span>;
  if (status === "tight") return <span className="hub-fit tight">Tight</span>;
  if (status === "no") return <span className="hub-fit no">Too large</span>;
  return <span className="hub-fit unknown">—</span>;
}

function compatDot(level: "ok" | "bad" | "unknown") {
  const title =
    level === "ok"
      ? "At least one quant fits your RAM"
      : level === "bad"
        ? "No quant fits your available RAM"
        : "Compatibility unknown";
  return <span className={`hub-compat-dot ${level}`} title={title} aria-label={title} />;
}

function progressCell(d: DownloadProgress | undefined) {
  if (!d) return null;
  const pct = d.totalBytes > 0 ? Math.round((d.receivedBytes / d.totalBytes) * 100) : 0;
  const speed = formatSpeed(d.speedBps);
  const eta = formatEta(d.etaSeconds);
  const shardLabel = d.parts && d.parts > 1 ? `shard ${d.part ?? 1}/${d.parts} · ` : "";
  return (
    <div className="hub-progress">
      {d.done ? (
        <span className="hub-dl-status done" title={d.checksumWarning ? "Checksum mismatch" : undefined}>
          Complete{d.checksumWarning ? " ⚠" : ""}
        </span>
      ) : d.cancelled ? (
        <span className="hub-dl-status">Cancelled</span>
      ) : d.paused ? (
        <span className="hub-dl-status paused">Paused</span>
      ) : d.error ? (
        <span className="hub-dl-status error" title={d.error}>Error</span>
      ) : (
        <>
          <span className="hub-dl-pct">{shardLabel}{pct}%</span>
          <div className="hub-progress-track">
            <div className="hub-progress-fill" style={{ width: `${pct}%` }} />
          </div>
          {speed ? <span className="hub-dl-rate">{speed}{eta ? ` · ${eta}` : ""}</span> : null}
        </>
      )}
    </div>
  );
}

function HuggingFaceIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <path d="M12 2c-1.5 2.2-3.8 3.6-6.2 4.1.4 3.1 1.8 6 4.2 8.2C8.5 17.8 10.1 20 12 22c1.9-2 3.5-4.2 5-7.7 2.4-2.2 3.8-5.1 4.2-8.2C15.8 5.6 13.5 4.2 12 2z" />
    </svg>
  );
}

function ModelHubPage() {
  const navigate = useNavigate();
  const { downloads, download, pause, resume, cancel, dismiss, loadModel } = useDownloadManager();
  const [query, setQuery] = useState(() => localStorage.getItem("hf-last-query") || "llama gguf");
  const [sortBy, setSortBy] = useState<SortKey>(() => {
    const stored = localStorage.getItem("hf-sort") as SortKey | null;
    return stored && stored in SORT_LABELS ? stored : "downloads";
  });
  const [results, setResults] = useState<HfResult[]>([]);
  const [openRepo, setOpenRepo] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState("");
  const [sys, setSys] = useState<SystemInfo | null>(null);
  const [downloaded, setDownloaded] = useState<Record<string, boolean>>({});
  const [note, setNote] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<{ repoId: string; file: string } | null>(null);
  const [tokenStatus, setTokenStatus] = useState<{ present: boolean; source?: string | null } | null>(null);
  const [tokenInput, setTokenInput] = useState("");
  const [loadingMore, setLoadingMore] = useState(false);
  const [hasMore, setHasMore] = useState(false);
  const [typeFilter, setTypeFilter] = useState<"any" | "instruct" | "base">("any");
  const [quantFilter, setQuantFilter] = useState<string>("any");
  const [sizeFilter, setSizeFilter] = useState<string>("any");
  const [authorFilter, setAuthorFilter] = useState("");
  const [licenseFilter, setLicenseFilter] = useState("");
  const [busyRow, setBusyRow] = useState<string | null>(null);
  const [sidebarFamily, setSidebarFamily] = useState<string | null>(null);
  const [showTokenPanel, setShowTokenPanel] = useState(false);

  const loadDownloaded = useCallback(async () => {
    try {
      const list = await api.listDownloadedModels();
      const map: Record<string, boolean> = {};
      for (const m of list) map[dlKey(m.repoId, m.fileName)] = true;
      setDownloaded(map);
    } catch {
      setDownloaded({});
    }
  }, []);

  const refreshSystem = useCallback(() => {
    void api.systemInfo().then(setSys).catch(() => {});
  }, []);

  useEffect(() => {
    void loadDownloaded();
    void api.hfTokenStatus().then(setTokenStatus).catch(() => setTokenStatus({ present: false }));
  }, [loadDownloaded]);

  useEffect(() => {
    void handleSearch();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    let timer: number | undefined;
    const tick = () => {
      if (document.visibilityState === "visible") refreshSystem();
    };
    tick();
    timer = window.setInterval(tick, SYSTEM_POLL_MS);
    return () => {
      if (timer !== undefined) window.clearInterval(timer);
    };
  }, [refreshSystem]);

  useEffect(() => {
    const doneKeys = Object.values(downloads).filter((d) => d.done).map((d) => dlKey(d.repoId, d.file));
    if (doneKeys.length > 0) void loadDownloaded();
  }, [downloads, loadDownloaded]);

  const persistQuery = (q: string) => {
    setQuery(q);
    localStorage.setItem("hf-last-query", q);
  };

  const handleSearch = useCallback(
    async (q?: string) => {
      const raw = (q ?? query).trim();
      const searchQuery = normalizeHubSearchQuery(raw) || "llama gguf";
      setSearching(true);
      setError("");
      setResults([]);
      setHasMore(false);
      setQuery(raw || searchQuery);
      localStorage.setItem("hf-last-query", raw || searchQuery);
      try {
        const res = (await api.hfSearch(searchQuery, PAGE_SIZE, 0, sortBy)) as HfResult[];
        setResults(res);
        setHasMore(res.length >= PAGE_SIZE);
        setOpenRepo(res.length > 0 ? res[0].repoId : null);
        if (res.length === 0) setError("No GGUF models found for that query.");
      } catch (err) {
        setError(String(err instanceof Error ? err.message : err));
      }
      setSearching(false);
    },
    [query, sortBy],
  );

  const loadMore = async () => {
    if (loadingMore || !hasMore) return;
    setLoadingMore(true);
    try {
      const res = (await api.hfSearch(query, PAGE_SIZE, results.length, sortBy)) as HfResult[];
      const seen = new Set(results.map((r) => r.repoId));
      const merged = [...results, ...res.filter((r) => !seen.has(r.repoId))];
      setResults(merged);
      setHasMore(res.length >= PAGE_SIZE && merged.length > results.length);
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
    }
    setLoadingMore(false);
  };

  const refreshTokenStatus = async () => {
    try {
      const st = await api.hfTokenStatus();
      setTokenStatus(st);
      return st;
    } catch {
      setTokenStatus({ present: false });
      return { present: false } as { present: boolean; source?: string | null };
    }
  };

  const saveToken = async () => {
    const t = tokenInput.trim();
    if (!t) return;
    try {
      await api.hfTokenSave(t);
      setTokenInput("");
      await refreshTokenStatus();
      setNote("Hugging Face token saved.");
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
    }
  };

  const clearToken = async () => {
    try {
      await api.hfTokenClear();
      await refreshTokenStatus();
      setNote("Hugging Face token removed.");
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
    }
  };

  const deleteModel = async (repoId: string, file: string) => {
    const key = dlKey(repoId, file);
    setBusyRow(key);
    try {
      const r = await api.deleteDownloadedModel(repoId, file);
      setNote(`Deleted ${file} (${formatBytes(r.freedBytes)} freed).`);
      await loadDownloaded();
      dismiss(repoId, file);
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
    }
    setBusyRow(null);
  };

  const loadDownloadedModel = async (repoId: string, file: string) => {
    const key = dlKey(repoId, file);
    setBusyRow(key);
    try {
      await loadModel(repoId, file);
      setNote(`Loading ${file.split("/").pop()}…`);
      navigate("/");
    } catch (err) {
      setError(`Could not load model: ${err instanceof Error ? err.message : String(err)}`);
    }
    setBusyRow(null);
  };

  const handleSort = (k: SortKey) => {
    setSortBy(k);
    localStorage.setItem("hf-sort", k);
    void handleSearch();
  };

  const sortedResults = useMemo(() => {
    if (sortBy !== "relevance") return results;
    const available = sys?.availableRamGb || 0;
    const arr = [...results];
    const compatRank = (files: HfResult["files"]) =>
      compatLevel(available, files) === "ok" ? 0 : compatLevel(available, files) === "bad" ? 1 : 2;
    arr.sort((a, b) => compatRank(a.files) - compatRank(b.files));
    return arr;
  }, [results, sys, sortBy]);

  const filteredResults = useMemo(() => {
    return sortedResults.filter((m) => {
      if (typeFilter === "instruct" && !isInstructModel(m)) return false;
      if (typeFilter === "base" && isInstructModel(m)) return false;
      if (authorFilter && !(m.author || m.repoId.split("/")[0] || "").toLowerCase().includes(authorFilter.toLowerCase()))
        return false;
      if (licenseFilter) {
        const lic = `${m.license || ""} ${(m.tags || []).join(" ")}`.toLowerCase();
        if (!lic.includes(licenseFilter.toLowerCase())) return false;
      }
      if (sizeFilter !== "any") {
        const totalGb = m.files.reduce((acc, f) => acc + (f.size || 0), 0) / GB;
        if (!totalGb) return false;
        const [op, limit] = sizeFilter.split(":");
        const n = Number(limit);
        if (op === "lt" && totalGb > n) return false;
        if (op === "gt" && totalGb < n) return false;
      }
      if (quantFilter !== "any") {
        if (!m.files.some((f) => parseQuant(f.name).startsWith(quantFilter))) return false;
      }
      return true;
    });
  }, [sortedResults, typeFilter, authorFilter, licenseFilter, sizeFilter, quantFilter]);

  const gatedVisible = filteredResults.some((m) => isGatedModel(m));
  const diskFreeBytes = (sys?.diskFreeGb || 0) * GB;

  const handleDownloadAction = async (repoId: string, file: string) => {
    const key = dlKey(repoId, file);
    const d = downloads[key];
    if (d?.done || downloaded[key]) return;
    if (d?.paused) {
      void resume(repoId, file);
      return;
    }
    if (d && !d.cancelled && !d.error) {
      void pause(repoId, file);
      return;
    }
    try {
      const model = results.find((r) => r.repoId === repoId);
      await download(repoId, file, model?.sha || null);
      setNote(`Downloading ${file.split("/").pop()}…`);
    } catch (err) {
      setError(`Download failed: ${err instanceof Error ? err.message : String(err)}`);
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

  const handleDownloadAll = async (model: HfResult) => {
    let diskFree = diskFreeBytes;
    let started = 0;
    for (const unit of downloadUnits(model.files)) {
      const key = dlKey(model.repoId, unit.name);
      if (!downloaded[key] && !downloads[key] && unit.size <= diskFree) {
        try {
          await download(model.repoId, unit.name, model.sha || null);
          started += 1;
          diskFree -= unit.size;
        } catch (err) {
          setError(`Download failed: ${err instanceof Error ? err.message : String(err)}`);
        }
      }
    }
    if (started > 0) setNote(`Started ${started} download(s).`);
    else if (!error) setNote("Nothing to download — already on disk or insufficient space.");
  };

  const openOnHF = (repoId: string) => {
    void api.openUrl(`https://huggingface.co/${repoId}`).catch((err) =>
      setError(`Couldn't open browser: ${String(err)}`),
    );
  };

  const pickFamily = (label: string, familyQuery: string) => {
    setSidebarFamily(label);
    persistQuery(familyQuery);
    void handleSearch(familyQuery);
  };

  return (
    <div className="hub-page agents-page">
      <header className="agents-hero">
        <div className="agents-hero-text">
          <h1 className="agents-title">Download models</h1>
          <p className="agents-subtitle">
            Search Hugging Face for GGUF weights used by the local agent backend. Downloads land in your models
            folder — load them from{" "}
            <button type="button" className="hub-inline-link" onClick={() => navigate("/agents?focus=local")}>
              Local GGUF
            </button>
            .
          </p>
        </div>
      </header>

      <nav className="hub-mode-tabs" aria-label="Model catalog">
        <button type="button" className="hub-mode-tab active">Text (GGUF)</button>
        <button type="button" className="hub-mode-tab" onClick={() => navigate("/images")}>
          Images
        </button>
        <button type="button" className="hub-mode-tab" onClick={() => navigate("/video")}>
          Video
        </button>
        <button type="button" className="hub-mode-tab" onClick={() => navigate("/llama-cpp")}>
          llama.cpp
        </button>
      </nav>

      {note ? (
        <div className="model-connected-banner hub-banner" role="status">
          <span className="model-dot-connected" />
          <span>{note}</span>
          <button type="button" className="hub-banner-dismiss" onClick={() => setNote(null)} aria-label="Dismiss">
            ×
          </button>
        </div>
      ) : null}

      <section className="hub-discover" aria-label="Model search">
        <div className="hub-discover-search">
          <div className="hub-discover-input-wrap">
            <svg className="hub-discover-icon" width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
              <circle cx="11" cy="11" r="8" />
              <path d="m21 21-4.3-4.3" />
            </svg>
            <input
              id="hub-query"
              type="search"
              className="hub-discover-input"
              placeholder="Search models — keyword, user/model, or Hugging Face URL"
              value={query}
              onChange={(e) => persistQuery(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void handleSearch();
              }}
            />
          </div>
          <button type="button" className="hub-discover-search-btn" onClick={() => void handleSearch()} disabled={searching}>
            {searching ? "Searching…" : "Search"}
          </button>
        </div>

        <div className="hub-discover-toolbar">
          <div className="hub-discover-filters">
            <label className="hub-discover-filter">
              <span>Type</span>
              <select value={typeFilter} onChange={(e) => setTypeFilter(e.target.value as typeof typeFilter)}>
                <option value="any">Any</option>
                <option value="instruct">Instruct</option>
                <option value="base">Base</option>
              </select>
            </label>
            <label className="hub-discover-filter">
              <span>Quant</span>
              <select value={quantFilter} onChange={(e) => setQuantFilter(e.target.value)}>
                <option value="any">Any</option>
                <option value="Q4">Q4+</option>
                <option value="Q5">Q5+</option>
                <option value="Q6">Q6+</option>
                <option value="Q8">Q8</option>
                <option value="F16">F16</option>
              </select>
            </label>
            <label className="hub-discover-filter">
              <span>Size</span>
              <select value={sizeFilter} onChange={(e) => setSizeFilter(e.target.value)}>
                <option value="any">Any</option>
                <option value="lt:8">≤ 8 GB</option>
                <option value="lt:16">≤ 16 GB</option>
                <option value="lt:32">≤ 32 GB</option>
                <option value="gt:32">&gt; 32 GB</option>
              </select>
            </label>
            <label className="hub-discover-filter">
              <span>Author</span>
              <input value={authorFilter} onChange={(e) => setAuthorFilter(e.target.value)} placeholder="any" />
            </label>
            <label className="hub-discover-filter hub-discover-filter-grow">
              <span>License</span>
              <input value={licenseFilter} onChange={(e) => setLicenseFilter(e.target.value)} placeholder="any" />
            </label>
          </div>
          <div className="hub-discover-sort">
            {(Object.entries(SORT_LABELS) as [SortKey, string][]).map(([k, label]) => (
              <button
                key={k}
                type="button"
                className={`hub-discover-sort-btn ${sortBy === k ? "active" : ""}`}
                onClick={() => handleSort(k)}
              >
                {label}
              </button>
            ))}
          </div>
          <button
            type="button"
            className={`hub-discover-token-btn ${tokenStatus?.present ? "ok" : ""}`}
            onClick={() => setShowTokenPanel((v) => !v)}
          >
            {tokenStatus?.present ? "HF token ✓" : "HF token"}
          </button>
        </div>

        {showTokenPanel ? (
          <div className="hub-discover-token-panel">
            <input
              type="password"
              className="modal-input"
              value={tokenInput}
              onChange={(e) => setTokenInput(e.target.value)}
              placeholder="hf_… read token for gated models"
              onKeyDown={(e) => {
                if (e.key === "Enter") void saveToken();
              }}
            />
            <button type="button" className="btn-modal-primary" onClick={() => void saveToken()} disabled={!tokenInput.trim()}>
              Save
            </button>
            {tokenStatus?.present ? (
              <button type="button" className="btn-modal-secondary" onClick={() => void clearToken()}>
                Remove
              </button>
            ) : null}
          </div>
        ) : null}
      </section>

      <div className="model-modal-body hub-layout">
        <aside className="model-providers-panel hub-aside">
          <div className="model-providers-panel-title">This machine</div>
          {sys ? (
            <div className="hub-sys-grid">
              <SysItem level={null} label="OS" value={sys.os} />
              <SysItem level={null} label="CPU" value={`${sys.cores} cores`} title={sys.cpu} />
              <SysItem
                level={ramCapacityLevel(sys.totalRamGb)}
                label="RAM"
                value={`${sys.availableRamGb.toFixed(1)} / ${sys.totalRamGb.toFixed(1)} GB`}
              />
              <SysItem
                level={levelFor(sys.diskFreeGb, sys.diskTotalGb)}
                label="Disk"
                value={`${sys.diskFreeGb.toFixed(1)} GB free`}
              />
              {sys.gpu ? (
                <SysItem level={null} label="GPU" value={`${sys.gpu.vramGb.toFixed(0)} GB VRAM`} title={sys.gpu.name} />
              ) : (
                <SysItem
                  level={sys.gpuEnabled ? "ok" : "warn"}
                  label="GPU"
                  value={sys.gpuEnabled ? "None detected" : "CPU-only build"}
                />
              )}
            </div>
          ) : (
            <p className="models-desc hub-aside-muted">Reading system info…</p>
          )}
          <button type="button" className="model-refresh-btn hub-refresh-sys" onClick={refreshSystem} title="Refresh hardware">
            Refresh
          </button>

          <div className="model-providers-panel-title hub-aside-gap">Staff picks</div>
          <div className="model-providers-list hub-family-list">
            {POPULAR_FAMILIES.map((c) => (
              <button
                key={c.label}
                type="button"
                className={`model-provider-item ${sidebarFamily === c.label ? "active" : ""}`}
                onClick={() => pickFamily(c.label, c.query)}
              >
                <span className="model-provider-icon">
                  <HuggingFaceIcon />
                </span>
                <span className="model-provider-item-text">
                  <span className="model-provider-item-name">{c.label}</span>
                  <span className="model-provider-tag">{c.hint}</span>
                </span>
              </button>
            ))}
          </div>
        </aside>

        <div className="hub-results-pane">
          {gatedVisible && !tokenStatus?.present ? (
            <div className="hub-gated-banner" role="status">
              Gated models need a Hugging Face token — accept the license on the model page first.
            </div>
          ) : null}

          {error ? <p className="hub-error">{error}</p> : null}
          {searching ? <div className="hub-spinner-wrap"><span className="spinner" /></div> : null}

          {!searching && results.length === 0 && !error ? (
            <div className="hub-discover-empty">
              <p>Search by name or paste a <span className="hub-code">user/model</span> path.</p>
              <button type="button" className="btn-modal-secondary" onClick={() => void handleSearch("gguf")}>
                Browse top GGUF downloads
              </button>
            </div>
          ) : null}

          {!searching && results.length > 0 ? (
            <div className="hub-results-count">
              {filteredResults.length} model{filteredResults.length === 1 ? "" : "s"}
              <span className="hub-results-count-muted"> · {SORT_LABELS[sortBy]}</span>
            </div>
          ) : null}

          <div className="hub-lms-list">
            {filteredResults.map((model) => {
              const { min, max } = fileSizeRange(model.files);
              const level = compatLevel(sys?.availableRamGb || 0, model.files);
              const isOpen = openRepo === model.repoId;
              const gated = isGatedModel(model);
              const rec = recommendedUnit(downloadUnits(model.files), sys);
              const { author, name } = parseRepoDisplay(model.repoId);
              const desc = (model.description || "").trim();
              const units = downloadUnits(model.files).sort((a, b) => (a.size || 0) - (b.size || 0));
              return (
                <article key={model.repoId} className={`hub-lms-card ${isOpen ? "open" : ""}`}>
                  <button
                    type="button"
                    className="hub-lms-card-head"
                    aria-expanded={isOpen}
                    onClick={() => setOpenRepo(isOpen ? null : model.repoId)}
                  >
                    <div className="hub-lms-card-main">
                      <div className="hub-lms-title-row">
                        <span className="hub-lms-model-name">{name}</span>
                        {compatDot(level)}
                        {gated ? <span className="hub-lms-badge gated">Gated</span> : null}
                        {isInstructModel(model) ? <span className="hub-lms-badge">Instruct</span> : null}
                      </div>
                      {author ? <span className="hub-lms-author">{author}</span> : null}
                      {desc ? <p className="hub-lms-blurb">{desc}</p> : null}
                      <div className="hub-lms-stats">
                        <span>{compactNum(model.downloads)} downloads</span>
                        <span>{compactNum(model.likes)} likes</span>
                        <span>{formatBytes(min)}{max > min ? ` – ${formatBytes(max)}` : ""}</span>
                        {model.lastModified ? <span>Updated {model.lastModified.slice(0, 10)}</span> : null}
                        {rec ? <span className="hub-lms-rec">Pick {parseQuant(rec.name)}</span> : null}
                      </div>
                    </div>
                    <span className="hub-lms-chevron" aria-hidden="true">{isOpen ? "▾" : "▸"}</span>
                  </button>

                  {isOpen ? (
                    <div className="hub-lms-quants">
                      <div className="hub-lms-quants-head">
                        <span>Choose a quantization</span>
                        <div className="hub-lms-quants-actions">
                          <button type="button" className="hub-lms-link" onClick={() => openOnHF(model.repoId)}>
                            View on Hugging Face
                          </button>
                          {units.length > 1 ? (
                            <button
                              type="button"
                              className="hub-lms-link"
                              disabled={gated && !tokenStatus?.present}
                              onClick={() => {
                                if (gated && !tokenStatus?.present) {
                                  setError("Add a Hugging Face token for gated downloads.");
                                  return;
                                }
                                void handleDownloadAll(model);
                              }}
                            >
                              Download all
                            </button>
                          ) : null}
                        </div>
                      </div>
                      {units.map((unit) => {
                        const size = unit.size || 0;
                        const est = estimateRamGb(size, 8192);
                        const fit = fitForModel(size, sys, 8192);
                        const noDisk = size > diskFreeBytes;
                        const key = dlKey(model.repoId, unit.name);
                        const dl = downloads[key];
                        const done = (downloaded[key] && !dl) || dl?.done;
                        const label = actionLabel(model.repoId, unit.name);
                        const disabled =
                          (!done && noDisk && !dl) || (gated && !tokenStatus?.present && !done && !dl?.done);
                        const quant = parseQuant(unit.name);
                        const busy = busyRow === key;
                        const isRec = rec?.name === unit.name;
                        return (
                          <div key={unit.name} className={`hub-lms-quant-row ${isRec ? "recommended" : ""}`}>
                            <div className="hub-lms-quant-id">
                              <span className="hub-lms-quant-label">{quant}</span>
                              {isRec ? <span className="hub-lms-quant-rec">Recommended</span> : null}
                              {unit.split ? (
                                <span className="hub-lms-quant-sub">{unit.files.length} shards</span>
                              ) : (
                                <span className="hub-lms-quant-sub" title={unit.name}>
                                  {unit.name.split("/").pop()}
                                </span>
                              )}
                            </div>
                            <div className="hub-lms-quant-meta">
                              <span>{formatBytes(size)}</span>
                              <span>~{est > 0 ? `${est.toFixed(1)} GB RAM` : "—"}</span>
                              {fitBadge(fit)}
                            </div>
                            <div className="hub-lms-quant-dl">
                              {progressCell(dl)}
                              {done ? (
                                <>
                                  <button
                                    type="button"
                                    className="hub-lms-dl-btn primary"
                                    disabled={busy}
                                    onClick={() => void loadDownloadedModel(model.repoId, unit.name)}
                                  >
                                    Load
                                  </button>
                                  <button
                                    type="button"
                                    className="hub-lms-dl-btn"
                                    disabled={busy}
                                    onClick={() => void deleteModel(model.repoId, unit.name)}
                                  >
                                    Delete
                                  </button>
                                </>
                              ) : dl?.error ? (
                                <button
                                  type="button"
                                  className="hub-lms-dl-btn primary error"
                                  onClick={() => void handleDownloadAction(model.repoId, unit.name)}
                                  title={dl.error}
                                >
                                  Retry
                                </button>
                              ) : (
                                <button
                                  type="button"
                                  className={`hub-lms-dl-btn primary${label === "Pause" ? " pause" : ""}${label === "Resume" ? " resume" : ""}`}
                                  disabled={disabled}
                                  onClick={() => void handleDownloadAction(model.repoId, unit.name)}
                                >
                                  {label}
                                </button>
                              )}
                              {!done && dl && !dl.done && (dl.receivedBytes > 0 || dl.totalBytes > 0) ? (
                                <button
                                  type="button"
                                  className="hub-lms-dl-cancel"
                                  onClick={() => setConfirming({ repoId: model.repoId, file: unit.name })}
                                  title="Cancel"
                                >
                                  ×
                                </button>
                              ) : null}
                            </div>
                          </div>
                        );
                      })}
                    </div>
                  ) : null}
                </article>
              );
            })}
          </div>

          {!searching && hasMore ? (
            <div className="hub-load-more">
              <button type="button" className="btn-modal-secondary" onClick={() => void loadMore()} disabled={loadingMore}>
                {loadingMore ? "Loading…" : "Load more"}
              </button>
            </div>
          ) : null}
        </div>
      </div>

      <footer className="models-footer hub-footer">
        <button type="button" className="btn-modal-secondary" onClick={() => navigate("/agents?focus=local")}>
          Back to local backend
        </button>
        <div className="models-footer-spacer" />
        <button
          type="button"
          className="model-open-folder-btn"
          onClick={() => void api.openModelsDir().then((d) => setNote(`Opened ${d}`)).catch((e) => setError(String(e)))}
        >
          Open models folder
        </button>
      </footer>

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
