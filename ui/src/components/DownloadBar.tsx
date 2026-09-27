import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { useApp } from "../context/AppContext";
import { useDownloadManager, formatBytes, formatSpeed, formatEta } from "../lib/downloads";
import { api } from "../lib/ipc";
import CancelDownloadDialog from "./CancelDownloadDialog";
import "./DownloadBar.css";

function DownloadBar() {
  const navigate = useNavigate();
  const { modelLoadParams, rememberLastModel } = useApp();
  const { downloads, active, completed, pause, resume, cancel, dismiss, loadModel } = useDownloadManager();
  const [loadingKey, setLoadingKey] = useState<string | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<{ repoId: string; file: string } | null>(null);

  const rows = active.length > 0 || completed.length > 0;
  if (!rows && !loadingKey) return null;

  const handleLoad = async (repoId: string, file: string) => {
    const key = `${repoId}::${file}`;
    setLoadingKey(key);
    setLoadError(null);
    try {
      await loadModel(repoId, file, modelLoadParams());
      const list = await api.listDownloadedModels();
      const model = list.find((m) => m.repoId === repoId && m.fileName === file);
      if (model) rememberLastModel({ kind: "local", name: model.fileName, path: model.path });
    } catch (err) {
      setLoadError(String(err instanceof Error ? err.message : err));
    }
    setLoadingKey(null);
  };

  return (
    <div className="download-bar" role="status" aria-live="polite">
      {loadingKey && <div className="download-bar-loading">Fitting model in memory — this can take a few seconds…</div>}
      {loadError && <div className="download-bar-error">{loadError} (see Models page)</div>}
      <div className="download-bar-inner">
        {active.map((d) => {
          const pct = d.totalBytes > 0 ? Math.round((d.receivedBytes / d.totalBytes) * 100) : 0;
          const speed = formatSpeed(d.speedBps);
          const eta = formatEta(d.etaSeconds);
          return (
            <div key={`${d.repoId}::${d.file}`} className="download-bar-row">
              <span className="download-bar-file" title={`${d.repoId} / ${d.file}`}>{d.file}</span>
              <span className="download-bar-pct">{pct}%</span>
              <div className="download-bar-track">
                <div className="download-bar-fill" style={{ width: `${pct}%` }} />
              </div>
              <span className="download-bar-bytes">{formatBytes(d.receivedBytes)} / {formatBytes(d.totalBytes)}</span>
              {speed && <span className="download-bar-speed">{speed}{eta ? ` · ${eta}` : ""}</span>}
              {d.paused ? (
                <button type="button" className="download-bar-btn" onClick={() => void resume(d.repoId, d.file)} title="Resume">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><polygon points="5,3 19,12 5,21" /></svg>
                </button>
              ) : (
                <button type="button" className="download-bar-btn" onClick={() => void pause(d.repoId, d.file)} title="Pause">
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><rect x="6" y="4" width="4" height="16" /><rect x="14" y="4" width="4" height="16" /></svg>
                </button>
              )}
              <button type="button" className="download-bar-btn download-bar-cancel" onClick={() => setConfirming({ repoId: d.repoId, file: d.file })} title="Cancel">
                <svg width="12" height="12" viewBox="0 0 12 12"><line x1="2" y1="2" x2="10" y2="10" stroke="currentColor" strokeWidth="1.5"/><line x1="10" y1="2" x2="2" y2="10" stroke="currentColor" strokeWidth="1.5"/></svg>
              </button>
            </div>
          );
        })}

        {completed.map((d) => (
          <div key={`${d.repoId}::${d.file}`} className="download-bar-row success">
            <span className="download-bar-check" title="Download complete">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round"><path d="M20 6 9 17l-5-5" /></svg>
            </span>
            <span className="download-bar-file" title={`${d.repoId} / ${d.file}`}>{d.file}</span>
            <span className="download-bar-done-label">Downloaded</span>
            <button
              type="button"
              className="download-bar-load"
              disabled={loadingKey !== null}
              onClick={() => void handleLoad(d.repoId, d.file)}
            >
              {loadingKey === `${d.repoId}::${d.file}` ? "Loading…" : "Load model"}
            </button>
            <button type="button" className="download-bar-link" onClick={() => navigate("/models")}>
              Go to Models
            </button>
            <button type="button" className="download-bar-btn download-bar-cancel" onClick={() => void dismiss(d.repoId, d.file)} title="Dismiss">
              <svg width="12" height="12" viewBox="0 0 12 12"><line x1="2" y1="2" x2="10" y2="10" stroke="currentColor" strokeWidth="1.5"/><line x1="10" y1="2" x2="2" y2="10" stroke="currentColor" strokeWidth="1.5"/></svg>
            </button>
          </div>
        ))}
      </div>

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

export default DownloadBar;