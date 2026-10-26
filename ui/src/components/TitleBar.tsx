import { getCurrentWindow } from "@tauri-apps/api/window";
import { useShell, useModel } from "../context/AppContext";
import { PROVIDERS } from "../lib/providers";
import "./TitleBar.css";

function TitleBar() {
  const { theme, setTheme } = useShell();
  const { modelName, modelProvider, modelLoadStage, modelLoadProgress, modelLoading, unloadModel, apiServerStatus } = useModel();
  const win = getCurrentWindow();

  const providerLabel = PROVIDERS.find((p) => p.id === modelProvider)?.name || modelProvider || "Local";
  const serverOff = Boolean(modelName) && !modelLoading && !apiServerStatus.running;

  const darkThemes = ["dark", "midnight", "ocean", "forest"];
  const effectiveDark = theme === "system"
    ? window.matchMedia("(prefers-color-scheme: dark)").matches
    : darkThemes.includes(theme);

  const handleMinimize = () => win.minimize();
  const handleMaximize = () => win.toggleMaximize();
  const handleClose = () => win.close();
  const handleToggleTheme = () => setTheme(effectiveDark ? "light" : "dark");

  const pct = modelLoading && modelLoadProgress != null ? Math.round(modelLoadProgress * 100) : null;
  const stageLabel = modelLoadStage === "error" ? "" : modelLoadStage || "load";

  return (
    <div className="titlebar" data-tauri-drag-region>
      <div className="titlebar-drag" data-tauri-drag-region />
      <div className="titlebar-brand" data-tauri-drag-region>
        <span className="titlebar-brand-name">Vencoder</span>
        <span className="titlebar-brand-subtitle">your personal AI assistant</span>
      </div>
      <div className="titlebar-model-status" data-tauri-drag-region>
        {modelLoading ? (
          <div className="titlebar-model-pill titlebar-model-loading" data-tauri-drag-region title={pct != null ? `Loading model… ${pct}%` : "Loading model…"}>
            <span className="titlebar-spinner" />
            <span className="titlebar-loading-label">
              {pct != null ? `Loading model… ${pct}%` : `Loading model (${stageLabel})…`}
            </span>
            {pct != null && (
              <span className="titlebar-load-track">
                <span className="titlebar-load-fill" style={{ width: `${Math.min(100, Math.max(0, pct))}%` }} />
              </span>
            )}
          </div>
        ) : modelName ? (
          <div
            className="titlebar-model-pill"
            data-tauri-drag-region
            title={
              serverOff
                ? `Loaded: ${modelName} · Provider: ${providerLabel} · Local server not running`
                : `Loaded: ${modelName} · Provider: ${providerLabel}`
            }
          >
            <span className={`titlebar-model-dot${serverOff ? " titlebar-model-dot--off" : ""}`} />
            <span className="titlebar-model-provider">{providerLabel}</span>
            <span className="titlebar-model-name">{modelName}</span>
            {serverOff && <span className="titlebar-server-off-label">server off</span>}
            <button
              type="button"
              className="titlebar-unload-btn"
              onClick={() => void unloadModel()}
              title="Unload current model"
            >
              <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4">
                <polyline points="3 6 5 6 21 6" />
                <path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2" />
                <path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6" />
                <line x1="10" y1="11" x2="10" y2="17" />
                <line x1="14" y1="11" x2="14" y2="17" />
              </svg>
              Unload
            </button>
          </div>
        ) : null}
      </div>
      <div className="titlebar-actions">
        <button
          type="button"
          className="titlebar-btn titlebar-theme-toggle"
          onClick={handleToggleTheme}
          title={`Switch to ${effectiveDark ? "light" : "dark"} theme`}
        >
          {effectiveDark ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <circle cx="12" cy="12" r="5" />
              <line x1="12" y1="1" x2="12" y2="3" />
              <line x1="12" y1="21" x2="12" y2="23" />
              <line x1="4.22" y1="4.22" x2="5.64" y2="5.64" />
              <line x1="18.36" y1="18.36" x2="19.78" y2="19.78" />
              <line x1="1" y1="12" x2="3" y2="12" />
              <line x1="21" y1="12" x2="23" y2="12" />
              <line x1="4.22" y1="19.78" x2="5.64" y2="18.36" />
              <line x1="18.36" y1="5.64" x2="19.78" y2="4.22" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z" />
            </svg>
          )}
        </button>
        <button
          type="button"
          className="titlebar-btn"
          onClick={handleMinimize}
          title="Minimize"
          aria-label="Minimize"
        >
          <svg width="12" height="12" viewBox="0 0 12 12">
            <rect x="2" y="5.5" width="8" height="1" fill="currentColor" />
          </svg>
        </button>
        <button
          type="button"
          className="titlebar-btn"
          onClick={handleMaximize}
          title="Maximize"
          aria-label="Maximize"
        >
          <svg width="12" height="12" viewBox="0 0 12 12">
            <rect x="2" y="2" width="8" height="8" rx="1" fill="none" stroke="currentColor" strokeWidth="1.2" />
          </svg>
        </button>
        <button
          type="button"
          className="titlebar-btn titlebar-btn-close"
          onClick={handleClose}
          title="Close"
          aria-label="Close window"
        >
          <svg width="12" height="12" viewBox="0 0 12 12">
            <line x1="3" y1="3" x2="9" y2="9" stroke="currentColor" strokeWidth="1.3" />
            <line x1="9" y1="3" x2="3" y2="9" stroke="currentColor" strokeWidth="1.3" />
          </svg>
        </button>
      </div>
    </div>
  );
}

export default TitleBar;