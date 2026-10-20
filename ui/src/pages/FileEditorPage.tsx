import { useEffect, useState } from "react";
import { useSearchParams } from "react-router-dom";
import { useShell, useContextPane } from "../context/AppContext";
import { api, type OpenEditorTarget } from "../lib/ipc";
import FileTreePanel from "../components/editor/FileTreePanel";
import EditorPane from "../components/editor/EditorPane";
import "./FileEditorPage.css";

function FileEditorPage() {
  const { projectPath, setProjectPath } = useShell();
  const { addContextPath } = useContextPane();
  const [searchParams] = useSearchParams();
  const [selected, setSelected] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [focusLine, setFocusLine] = useState<number | null>(null);

  const pathParam = searchParams.get("path");
  useEffect(() => {
    if (pathParam) setSelected(pathParam);
  }, [pathParam]);

  const lineParam = searchParams.get("line");
  useEffect(() => {
    setFocusLine(lineParam ? Number(lineParam) : null);
  }, [lineParam]);

  useEffect(() => {
    const listener = (e: Event) => {
      const detail = (e as CustomEvent<OpenEditorTarget | null>).detail;
      if (!detail) return;
      const path = typeof detail === "string" ? detail : detail.path;
      const line = typeof detail === "string" ? undefined : detail.line;
      if (path) {
        setSelected(path);
        setFocusLine(line ?? null);
      }
    };
    window.addEventListener("vencoder-open-editor", listener);
    return () => window.removeEventListener("vencoder-open-editor", listener);
  }, []);

  const handleBrowse = async () => {
    try {
      const path = await api.pickWorkspaceFolder();
      if (path) {
        setProjectPath(path);
        await api.agentSetWorkspace(path);
      }
    } catch {
      // dialog dismissed
    }
  };

  return (
    <div className="editor-view">
      <div className="editor-header">
        <div className="project-header-row">
          <label className="project-label">Workspace</label>
          <button
            type="button"
            className="project-path project-path-full"
            onClick={() => void handleBrowse()}
            title="Click to change project folder"
          >
            <span className="project-path-text">{projectPath || "No folder selected"}</span>
          </button>
        </div>
        <div className="project-header-row project-header-row-buttons">
          <button
            type="button"
            className="btn-index"
            onClick={() => setReload((r) => r + 1)}
            disabled={!projectPath}
          >
            Refresh
          </button>
        </div>
      </div>
      <div className="editor-explorer">
        <FileTreePanel
          projectPath={projectPath}
          selectedPath={selected}
          onSelect={(p) => {
            setSelected(p);
            setFocusLine(null);
          }}
          onOpenHit={(p, line) => {
            setSelected(p);
            setFocusLine(line);
          }}
          refreshSignal={reload}
        />
        <EditorPane
          selectedPath={selected}
          projectPath={projectPath}
          onAddContext={(rel) => addContextPath(rel)}
          focusLine={focusLine}
        />
      </div>
    </div>
  );
}

export default FileEditorPage;