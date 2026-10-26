import { useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useShell } from "../context/AppContext";
import { api, isTauriRuntime } from "../lib/ipc";
import "./WorkspaceSwitcher.css";

function baseName(p: string): string {
  const parts = p.split(/[/\\]/).filter(Boolean);
  return parts[parts.length - 1] || p;
}

function WorkspaceSwitcher() {
  const { projectPath, setProjectPath, recentProjects } = useShell();
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [registered, setRegistered] = useState<string[]>([]);
  const rootRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .agentGetWorkspaces()
      .then(setRegistered)
      .catch(() => {});
  }, [projectPath]);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const roots = Array.from(new Set([...recentProjects, ...registered]));

  const choose = (p: string) => {
    if (p !== projectPath) setProjectPath(p);
    setOpen(false);
    navigate("/workspace");
  };

  const browse = async () => {
    try {
      const p = await api.pickWorkspaceFolder();
      if (p) choose(p);
      else setOpen(false);
    } catch {
      setOpen(false);
    }
  };

  return (
    <div className="ws-switcher" ref={rootRef}>
      <button
        type="button"
        className="ws-switcher-btn"
        onClick={() => setOpen((v) => !v)}
        title={projectPath || "No workspace open"}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <span className="ws-switcher-label">{projectPath ? baseName(projectPath) : "No workspace"}</span>
        <span className="ws-switcher-caret" aria-hidden="true">▾</span>
      </button>
      {open && (
        <div className="ws-switcher-menu" role="listbox" aria-label="Recent workspaces">
          {roots.length === 0 && <div className="ws-switcher-empty">No recent workspaces</div>}
          {roots.map((p) => (
            <button
              key={p}
              type="button"
              role="option"
              aria-selected={p === projectPath}
              className={`ws-switcher-item${p === projectPath ? " active" : ""}`}
              onClick={() => choose(p)}
              title={p}
            >
              <span className="ws-switcher-item-name">{baseName(p)}</span>
              <span className="ws-switcher-item-path">{p}</span>
            </button>
          ))}
          <div className="ws-switcher-sep" />
          <button type="button" className="ws-switcher-item ws-switcher-open" onClick={() => void browse()}>
            Open folder…
          </button>
        </div>
      )}
    </div>
  );
}

export default WorkspaceSwitcher;
