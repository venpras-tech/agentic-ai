import { useCallback, useEffect, useState } from "react";
import { api } from "../../lib/ipc";
import "./editor.css";

interface FsEntry {
  name: string;
  path: string;
  isDir: boolean;
}

interface FileTreePanelProps {
  projectPath: string | null;
  selectedPath: string | null;
  onSelect: (path: string) => void;
  refreshSignal?: number;
}

const ROOT_KEY = "__root__";
const SKIPPED_DIRS = new Set(["node_modules", ".git", "target", "dist", ".next", ".venv", "__pycache__"]);

function FileTreePanel({ projectPath, selectedPath, onSelect, refreshSignal = 0 }: FileTreePanelProps) {
  const [dirs, setDirs] = useState<Record<string, FsEntry[]>>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  const loadDir = useCallback(
    async (parentPath: string) => {
      if (!projectPath) return;
      try {
        const entries = await api.listDirectory(projectPath, parentPath === ROOT_KEY ? null : parentPath);
        setDirs((prev) => ({
          ...prev,
          [parentPath]: entries
            .filter((e) => !(e.isDir && SKIPPED_DIRS.has(e.name)))
            .sort((a, b) => (a.isDir === b.isDir ? a.name.localeCompare(b.name) : a.isDir ? -1 : 1)),
        }));
      } catch {
        setDirs((prev) => ({ ...prev, [parentPath]: [] }));
      }
    },
    [projectPath],
  );

  useEffect(() => {
    if (projectPath) {
      setDirs({});
      setExpanded({});
      void loadDir(ROOT_KEY);
    }
  }, [projectPath, refreshSignal, loadDir]);

  const toggleDir = useCallback(
    (path: string) => {
      setExpanded((prev) => {
        const next = { ...prev, [path]: !prev[path] };
        if (next[path] && !dirs[path]) void loadDir(path);
        return next;
      });
    },
    [dirs, loadDir],
  );

  const renderEntries = (parentKey: string, depth: number) => {
    const entries = dirs[parentKey] ?? [];
    return entries.map((entry) => {
      const isOpen = Boolean(expanded[entry.path]);
      return (
        <div key={entry.path}>
          <div
            className={`editor-tree-row${entry.isDir ? " dir" : ""}${selectedPath === entry.path ? " selected" : ""}`}
            style={{ paddingLeft: 8 + depth * 14 }}
            onClick={() => (entry.isDir ? toggleDir(entry.path) : onSelect(entry.path))}
            title={entry.path}
          >
            <span className="editor-tree-caret">{entry.isDir ? (isOpen ? "▾" : "▸") : ""}</span>
            <span className="editor-tree-icon">{entry.isDir ? "📁" : "📄"}</span>
            <span className="editor-tree-name">{entry.name}</span>
          </div>
          {entry.isDir && isOpen && renderEntries(entry.path, depth + 1)}
        </div>
      );
    });
  };

  return (
    <div className="editor-tree-panel">
      <div className="editor-tree-header">Files</div>
      <div className="editor-tree">
        {!projectPath ? (
          <div className="editor-tree-loading">No workspace selected</div>
        ) : (dirs[ROOT_KEY]?.length ?? 0) === 0 ? (
          <div className="editor-tree-loading">Empty workspace</div>
        ) : (
          renderEntries(ROOT_KEY, 0)
        )}
      </div>
    </div>
  );
}

export default FileTreePanel;