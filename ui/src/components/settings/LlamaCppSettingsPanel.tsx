import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { useModel } from "../../context/AppContext";
import {
  api,
  isTauriRuntime,
  type LlamaBindingsUpdateInfo,
  type LlamaLoadConfig,
  type LlamaRuntimeInfo,
  type MediaEngineSettings,
} from "../../lib/ipc";
import {
  applyClearedConfig,
  mergeLocalConfig,
  normalizedConfig,
  type LocalModelConfig,
} from "../../lib/localModelConfig";
import {
  changedFields,
  estimateKvBytes,
  formatBytes,
  requiresContextRebuild,
  LLAMA_EDIT_FIELDS,
} from "../../lib/llamaCpp";
import "./LlamaCppSettingsPanel.css";

function LNum({
  id,
  label,
  value,
  onChange,
  min,
  max,
  step,
  placeholder,
  hint,
}: {
  id: string;
  label: string;
  value?: number;
  onChange: (v: number | null) => void;
  min?: number;
  max?: number;
  step?: number;
  placeholder?: string;
  hint?: string;
}) {
  return (
    <div className="settings-llm-item">
      <label className="settings-llm-label" htmlFor={id}>{label}</label>
      <input
        id={id}
        type="number"
        className="settings-input"
        value={value ?? ""}
        min={min}
        max={max}
        step={step}
        placeholder={placeholder}
        onChange={(e) => {
          const raw = e.target.value.trim();
          if (!raw) {
            onChange(null);
            return;
          }
          const n = Number(raw);
          onChange(Number.isFinite(n) ? n : null);
        }}
      />
      {hint ? <span className="settings-llm-default">{hint}</span> : null}
    </div>
  );
}

function LlamaCppSettingsPanel({ embedded }: { embedded?: boolean }) {
  const {
    modelMeta,
    activeLocalModelPath,
    activeModelConfig,
    saveLocalModelConfig,
    reloadModel,
    modelLoading,
    llmSettings,
    saveLlmSettings,
  } = useModel();

  const [llamaInfo, setLlamaInfo] = useState<LlamaRuntimeInfo | null>(null);
  const [llamaDraft, setLlamaDraft] = useState<LlamaLoadConfig>({});
  const [gpuLayers, setGpuLayers] = useState<number | null>(null);
  const [threads, setThreads] = useState<number | null>(null);
  const [msg, setMsg] = useState<{ text: string; error?: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const [updateInfo, setUpdateInfo] = useState<LlamaBindingsUpdateInfo | null>(null);
  const [updateBusy, setUpdateBusy] = useState(false);
  const [media, setMedia] = useState<MediaEngineSettings>({});

  const loadLlamaInfo = useCallback(async () => {
    if (!isTauriRuntime()) return;
    try {
      const info = await api.llamaRuntimeInfo();
      setLlamaInfo(info);
      const c = info.loadConfig;
      const draft: LlamaLoadConfig = {};
      if (c) {
        for (const key of LLAMA_EDIT_FIELDS) {
          const v = (c as Record<string, unknown>)[key];
          if (v !== undefined && v !== null) (draft as Record<string, unknown>)[key] = v;
        }
      }
      setLlamaDraft(draft);
      setGpuLayers(c?.nGpuLayers ?? llmSettings.nGpuLayers ?? null);
      setThreads(c?.nThreads ?? llmSettings.nThreads ?? null);
    } catch {
      setLlamaInfo(null);
    }
  }, [llmSettings.nGpuLayers, llmSettings.nThreads]);

  useEffect(() => {
    void loadLlamaInfo();
    if (!isTauriRuntime()) return;
    void api.mediaEngineSettingsLoad().then(setMedia).catch(() => {});
  }, [loadLlamaInfo, modelMeta]);

  const checkUpdate = async () => {
    setUpdateBusy(true);
    setMsg(null);
    try {
      const info = await api.llamaBindingsUpdateCheck();
      setUpdateInfo(info);
      setMsg({
        text: info.updateAvailable
          ? `Update available: ${info.currentVersion} → ${info.latestVersion}`
          : `Bindings are up to date (${info.currentVersion}).`,
      });
    } catch (err) {
      setMsg({ text: err instanceof Error ? err.message : String(err), error: true });
    } finally {
      setUpdateBusy(false);
    }
  };

  const persistDraft = () => {
    const cfgPatch = normalizedConfig({
      ...llamaDraft,
      nGpuLayers: gpuLayers,
      nThreads: threads,
    } as Partial<LocalModelConfig>);
    const patch = mergeLocalConfig(activeModelConfig, cfgPatch);
    saveLocalModelConfig(
      activeLocalModelPath,
      applyClearedConfig(patch, {
        ...llamaDraft,
        nGpuLayers: gpuLayers,
        nThreads: threads,
      } as Record<string, unknown>),
    );
    saveLlmSettings({
      nGpuLayers: gpuLayers ?? undefined,
      nThreads: threads ?? undefined,
      contextSize: llamaDraft.contextSize ?? undefined,
    });
  };

  const applyAndReload = async () => {
    setBusy(true);
    setMsg(null);
    try {
      persistDraft();
      await api.mediaEngineSettingsSave(media);
      const info = await reloadModel();
      await loadLlamaInfo();
      setMsg({
        text: `Model reloaded — context ${(info?.contextSize ?? 0).toLocaleString()} tokens.`,
      });
    } catch (err) {
      setMsg({ text: err instanceof Error ? err.message : String(err), error: true });
    } finally {
      setBusy(false);
    }
  };

  const saveMediaOnly = async () => {
    try {
      await api.mediaEngineSettingsSave(media);
      setMsg({ text: "Media engine paths saved." });
    } catch (err) {
      setMsg({ text: err instanceof Error ? err.message : String(err), error: true });
    }
  };

  const resetDraft = () => {
    const c = llamaInfo?.loadConfig;
    const draft: LlamaLoadConfig = {};
    if (c) {
      for (const key of LLAMA_EDIT_FIELDS) {
        const v = (c as Record<string, unknown>)[key];
        if (v !== undefined && v !== null) (draft as Record<string, unknown>)[key] = v;
      }
    }
    setLlamaDraft(draft);
    setGpuLayers(c?.nGpuLayers ?? null);
    setThreads(c?.nThreads ?? null);
    setMsg(null);
  };

  const wrapClass = embedded ? "llama-panel llama-panel-embedded" : "llama-panel";

  return (
    <div className={wrapClass}>
      {!embedded ? (
        <header className="llama-panel-hero">
          <h2>llama.cpp</h2>
          <p>
            Local inference engine settings, bindings updates, and media-engine paths for image/video
            models (Atomic Chat–style workflow).
          </p>
        </header>
      ) : (
        <p className="settings-hint">
          Full control panel: <Link to="/llama-cpp">Open llama.cpp page</Link>
        </p>
      )}

      <div className="llama-panel-actions">
        <button type="button" className="settings-btn" disabled={busy || updateBusy} onClick={() => void loadLlamaInfo()}>
          Refresh
        </button>
        <button type="button" className="settings-btn" disabled={updateBusy} onClick={() => void checkUpdate()}>
          {updateBusy ? "Checking…" : "Check bindings update"}
        </button>
        <button
          type="button"
          className="settings-btn settings-btn-primary"
          disabled={busy || modelLoading || !activeLocalModelPath}
          onClick={() => void applyAndReload()}
        >
          {busy ? "Reloading…" : "Save & reload model"}
        </button>
        <button type="button" className="settings-btn settings-btn-danger" disabled={busy} onClick={resetDraft}>
          Reset draft
        </button>
      </div>

      {msg ? (
        <p className={`settings-hint${msg.error ? " settings-local-error" : ""}`} role="status">{msg.text}</p>
      ) : null}

      <section className="llama-section">
        <h3>Bindings version</h3>
        {llamaInfo ? (
          <div className="settings-about-meta">
            <span>Installed: llama-cpp-2 {llamaInfo.build.bindingsVersion}</span>
            {updateInfo ? (
              <span>
                Latest on crates.io: {updateInfo.latestVersion}
                {updateInfo.updateAvailable ? " (newer available)" : ""}
              </span>
            ) : null}
          </div>
        ) : (
          <p className="settings-hint">Run the desktop app to inspect the bundled engine.</p>
        )}
        {updateInfo?.updateAvailable ? (
          <p className="settings-hint">{updateInfo.note}</p>
        ) : null}
        {updateInfo ? (
          <div className="llama-link-row">
            <a href={updateInfo.cratesIoUrl} target="_blank" rel="noreferrer">crates.io</a>
            <a href={updateInfo.upstreamRepoUrl} target="_blank" rel="noreferrer">llama.cpp upstream</a>
          </div>
        ) : null}
      </section>

      <section className="llama-section">
        <h3>Build &amp; devices</h3>
        {llamaInfo ? (
          <div className="settings-about-meta">
            <span>
              GPU offload:{" "}
              {llamaInfo.build.supportsGpuOffload
                ? `yes (${llamaInfo.build.deviceCount} device${llamaInfo.build.deviceCount === 1 ? "" : "s"})`
                : "not in this binary"}
            </span>
            <span>GPU feature flag: {llamaInfo.build.gpuFeatureEnabled ? "on" : "CPU build"}</span>
            <span>
              mmap {llamaInfo.build.supportsMmap ? "yes" : "no"} · mlock {llamaInfo.build.supportsMlock ? "yes" : "no"} ·
              max devices {llamaInfo.build.maxDevices}
            </span>
            {llamaInfo.build.systemInfo ? (
              <span className="settings-llama-sysinfo" title={llamaInfo.build.systemInfo}>
                {llamaInfo.build.systemInfo}
              </span>
            ) : null}
          </div>
        ) : null}
      </section>

      <section className="llama-section">
        <h3>Media engine (images &amp; video)</h3>
        <p className="settings-hint">
          Cloud image generation uses your active API provider. Local Wan/LTX/SD checkpoints use these paths when the
          media engine is wired (configure now; generation UI is on Images / Video pages).
        </p>
        <div className="settings-llm-grid">
          <div className="settings-llm-item">
            <label className="settings-llm-label" htmlFor="media-bin">Local engine binary</label>
            <input
              id="media-bin"
              className="settings-input"
              value={media.localBinary ?? ""}
              placeholder="Path to media engine executable (optional)"
              onChange={(e) => setMedia((m) => ({ ...m, localBinary: e.target.value || null }))}
            />
          </div>
          <div className="settings-llm-item">
            <label className="settings-llm-label" htmlFor="media-img">Image model checkpoint</label>
            <input
              id="media-img"
              className="settings-input"
              value={media.imageModelPath ?? ""}
              placeholder="GGUF / engine model path"
              onChange={(e) => setMedia((m) => ({ ...m, imageModelPath: e.target.value || null }))}
            />
          </div>
          <div className="settings-llm-item">
            <label className="settings-llm-label" htmlFor="media-vid">Video model checkpoint</label>
            <input
              id="media-vid"
              className="settings-input"
              value={media.videoModelPath ?? ""}
              placeholder="Wan / LTX weights path"
              onChange={(e) => setMedia((m) => ({ ...m, videoModelPath: e.target.value || null }))}
            />
          </div>
        </div>
        <button type="button" className="settings-btn" onClick={() => void saveMediaOnly()}>
          Save media paths
        </button>
      </section>

      <section className="llama-section">
        <h3>Loaded model</h3>
        {llamaInfo?.remote ? (
          <p className="settings-hint">Active backend is remote — llama.cpp load options do not apply.</p>
        ) : llamaInfo?.model ? (
          <div className="settings-about-meta">
            <span>{llamaInfo.model.name} · {llamaInfo.model.architecture}</span>
            <span>
              Context {(llamaInfo.model.contextSize ?? 0).toLocaleString()} / train{" "}
              {llamaInfo.model.nCtxTrain.toLocaleString()}
            </span>
            <span>Layers {llamaInfo.model.nLayer} · workers {llamaInfo.workerCount ?? 1}</span>
            {llamaInfo.loadedPath ? (
              <span className="settings-llama-sysinfo" title={llamaInfo.loadedPath}>{llamaInfo.loadedPath}</span>
            ) : null}
          </div>
        ) : (
          <p className="settings-hint">Load a local GGUF model to tune runtime parameters.</p>
        )}
      </section>

      {llamaInfo?.model && !llamaInfo.remote ? (
        <>
          <section className="llama-section">
            <h3>Throughput</h3>
            <div className="settings-llm-grid">
              <LNum
                id="llama-gpu"
                label="GPU layers (n_gpu_layers)"
                value={gpuLayers ?? undefined}
                onChange={setGpuLayers}
                min={0}
                max={999}
                step={1}
                placeholder="auto"
                hint="-1 = all layers on GPU when supported."
              />
              <LNum
                id="llama-threads"
                label="CPU threads"
                value={threads ?? undefined}
                onChange={setThreads}
                min={1}
                max={128}
                step={1}
                placeholder="auto"
              />
              <LNum
                id="llama-ctx"
                label="Context length"
                value={llamaDraft.contextSize ?? undefined}
                onChange={(v) => setLlamaDraft((p) => ({ ...p, contextSize: v ?? null }))}
                min={128}
                max={Math.max(128, llamaInfo.model!.nCtxTrain)}
                step={256}
                placeholder="auto"
              />
              <LNum
                id="llama-batch"
                label="Batch (n_batch)"
                value={llamaDraft.nBatch ?? undefined}
                onChange={(v) => setLlamaDraft((p) => ({ ...p, nBatch: v ?? null }))}
                min={1}
                max={1048576}
                step={64}
                placeholder="512"
              />
              <LNum
                id="llama-ubatch"
                label="Micro-batch (n_ubatch)"
                value={llamaDraft.nUbatch ?? undefined}
                onChange={(v) => setLlamaDraft((p) => ({ ...p, nUbatch: v ?? null }))}
                min={1}
                max={1048576}
                step={64}
              />
              <LNum
                id="llama-workers"
                label="Parallel slots"
                value={llamaDraft.nWorkers ?? undefined}
                onChange={(v) => setLlamaDraft((p) => ({ ...p, nWorkers: v ?? null }))}
                min={1}
                max={32}
                step={1}
              />
              <LNum
                id="llama-seed"
                label="Default seed"
                value={llamaDraft.seed ?? undefined}
                onChange={(v) => setLlamaDraft((p) => ({ ...p, seed: v ?? null }))}
                min={0}
                max={4294967295}
                step={1}
                placeholder="random"
              />
            </div>
          </section>

          <section className="llama-section">
            <h3>KV cache &amp; memory</h3>
            <div className="settings-about-meta">
              <span>
                KV estimate:{" "}
                {formatBytes(
                  estimateKvBytes(
                    { nLayer: llamaInfo.model.nLayer, nEmbd: llamaInfo.model.nEmbd },
                    llamaDraft.contextSize ?? llamaInfo.model.contextSize ?? 0,
                    llamaDraft.cacheTypeK ?? llamaInfo.cacheTypeK,
                    llamaDraft.cacheTypeV ?? llamaInfo.cacheTypeV,
                  ),
                )}
              </span>
            </div>
            <div className="settings-llama-toggles">
              <label className="settings-check settings-check-inline">
                <input
                  type="checkbox"
                  checked={llamaDraft.flashAttn === true}
                  onChange={(e) => setLlamaDraft((p) => ({ ...p, flashAttn: e.target.checked }))}
                />
                <span>Flash attention</span>
              </label>
              <label className="settings-check settings-check-inline">
                <input
                  type="checkbox"
                  checked={llamaDraft.unifiedKv === true}
                  onChange={(e) => setLlamaDraft((p) => ({ ...p, unifiedKv: e.target.checked }))}
                />
                <span>Unified KV (uKVC)</span>
              </label>
              <label className="settings-check settings-check-inline">
                <input
                  type="checkbox"
                  checked={llamaDraft.offloadKqv === true}
                  onChange={(e) => setLlamaDraft((p) => ({ ...p, offloadKqv: e.target.checked }))}
                />
                <span>Offload KV to GPU</span>
              </label>
              <label className="settings-check settings-check-inline">
                <input
                  type="checkbox"
                  checked={llamaDraft.useMmap === true}
                  onChange={(e) => setLlamaDraft((p) => ({ ...p, useMmap: e.target.checked }))}
                />
                <span>mmap weights</span>
              </label>
              <label className="settings-check settings-check-inline">
                <input
                  type="checkbox"
                  checked={llamaDraft.useMlock === true}
                  onChange={(e) => setLlamaDraft((p) => ({ ...p, useMlock: e.target.checked }))}
                />
                <span>mlock</span>
              </label>
            </div>
            <div className="settings-llm-grid">
              <div className="settings-llm-item">
                <label className="settings-llm-label" htmlFor="llama-cache-k">K-cache type</label>
                <select
                  id="llama-cache-k"
                  className="settings-input"
                  value={llamaDraft.cacheTypeK ?? ""}
                  onChange={(e) => setLlamaDraft((p) => ({ ...p, cacheTypeK: e.target.value || null }))}
                >
                  <option value="">auto (F16)</option>
                  {["F16", "Q8_0", "Q4_0", "Q4_1", "Q5_0", "Q5_1", "IQ4_NL"].map((t) => (
                    <option key={t} value={t}>{t}</option>
                  ))}
                </select>
              </div>
              <div className="settings-llm-item">
                <label className="settings-llm-label" htmlFor="llama-cache-v">V-cache type</label>
                <select
                  id="llama-cache-v"
                  className="settings-input"
                  value={llamaDraft.cacheTypeV ?? ""}
                  onChange={(e) => setLlamaDraft((p) => ({ ...p, cacheTypeV: e.target.value || null }))}
                >
                  <option value="">auto (F16)</option>
                  {["F16", "Q8_0", "Q4_0", "Q4_1", "Q5_0", "Q5_1", "IQ4_NL"].map((t) => (
                    <option key={t} value={t}>{t}</option>
                  ))}
                </select>
              </div>
            </div>
          </section>

          <section className="llama-section">
            <h3>Advanced</h3>
            <div className="settings-llm-grid">
              <LNum
                id="llama-rope-base"
                label="RoPE base"
                value={llamaDraft.ropeFreqBase ?? undefined}
                onChange={(v) => setLlamaDraft((p) => ({ ...p, ropeFreqBase: v ?? null }))}
                min={0.1}
                max={1_000_000}
                step={100}
              />
              <LNum
                id="llama-rope-scale"
                label="RoPE scale"
                value={llamaDraft.ropeFreqScale ?? undefined}
                onChange={(v) => setLlamaDraft((p) => ({ ...p, ropeFreqScale: v ?? null }))}
                min={0.01}
                max={1000}
                step={0.05}
              />
            </div>
            <div className="settings-llm-item">
              <label className="settings-llm-label" htmlFor="llama-args">Raw llama.cpp CLI args</label>
              <input
                id="llama-args"
                className="settings-input"
                value={llamaDraft.llamaArgs ?? ""}
                onChange={(e) => setLlamaDraft((p) => ({ ...p, llamaArgs: e.target.value || null }))}
                placeholder="--no-mmap --flash-attn"
              />
            </div>
            {(() => {
              const changed = changedFields(llamaInfo.loadConfig, llamaDraft);
              if (!changed.length) return null;
              return (
                <p className="settings-hint">
                  Pending: {changed.join(", ")}.
                  {requiresContextRebuild(llamaInfo.loadConfig, llamaDraft)
                    ? " Context rebuild required — save & reload."
                    : " Save & reload to apply."}
                </p>
              );
            })()}
          </section>
        </>
      ) : null}
    </div>
  );
}

export default LlamaCppSettingsPanel;
