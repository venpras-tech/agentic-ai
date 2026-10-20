import { useCallback, useEffect, useState } from "react";
import { api, isTauriRuntime, type IndexSearchHit } from "../../lib/ipc";
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
  /** Open a search hit in-place (the same editor pane) at its line; falls back to `onSelect`. */
  onOpenHit?: (path: string, line: number) => void;
}

const ROOT_KEY = "__root__";
const SKIPPED_DIRS = new Set(["node_modules", ".git", "target", "dist", ".next", ".venv", "__pycache__"]);

function FileTreePanel({
  projectPath,
  selectedPath,
  onSelect,
  refreshSignal = 0,
  onOpenHit,
}: FileTreePanelProps) {
  const [dirs, setDirs] = useState<Record<string, FsEntry[]>>({});
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<IndexSearchHit[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState("");
  const [semantic, setSemantic] = useState(false);

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

  const runSearch = async () => {
    if (!projectPath || !query.trim()) return;
    setSearching(true);
    setSearchError("");
    try {
      const hits = semantic
        ? await api.indexSemanticSearch(projectPath, query.trim())
        : await api.indexSearch(projectPath, query.trim());
      setResults(hits);
    } catch (e) {
      setSearchError(e instanceof Error ? e.message : String(e));
      setResults(null);
    } finally {
      setSearching(false);
    }
  };

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

  const openHit = (hit: IndexSearchHit) => {
    if (!projectPath) return;
    const base = projectPath.replaceAll("\\", "/").replace(/\/?$/, "/");
    const abs = `${base}${hit.path}`;
    if (onOpenHit) onOpenHit(abs, hit.lineNumber);
    else onSelect(abs);
  };

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
      {isTauriRuntime() && (
        <div className="editor-tree-search">
          <div className="editor-tree-search-row">
            <input
              className="editor-tree-search-input"
              value={query}
              placeholder="Search & open files…"
              onChange={(e) => {
                setQuery(e.target.value);
                if (!e.target.value) setResults(null);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") void runSearch();
              }}
              disabled={!projectPath || searching}
              aria-label="Search workspace"
            />
            <button
              type="button"
              className="editor-tree-search-btn"
              onClick={() => void runSearch()}
              disabled={!projectPath || searching || !query.trim()}
            >
              {searching ? "…" : "Go"}
            </button>
            <label className="editor-tree-semantic" title="Keyword semantic search">
              <input
                type="checkbox"
                checked={semantic}
                onChange={(e) => setSemantic(e.target.checked)}
              />
              <span>Sem</span>
            </label>
          </div>
          {searchError && <div className="editor-tree-search-error">{searchError}</div>}
          {results !== null && (
            <div className="editor-tree-results">
              {results.length === 0 ? (
                <div className="editor-tree-loading">No matches.</div>
              ) : (
                results.map((h, i) => (
                  <button
                    key={`${h.path}:${h.lineNumber}:${i}`}
                    type="button"
                    className="editor-tree-result"
                    onClick={() => openHit(h)}
                    title={`Open ${h.path}:${h.lineNumber}`}
                  >
                    <span className="editor-tree-result-path">
                      {h.path}:{h.lineNumber}
                    </span>
                    <span className="editor-tree-result-line">{h.line}</span>
                  </button>
                ))
              )}
            </div>
          )}
        </div>
      )}
      {results !== null ? null : (
        <div className="editor-tree">
          {!projectPath ? (
            <div className="editor-tree-loading">No workspace selected</div>
          ) : (dirs[ROOT_KEY]?.length ?? 0) === 0 ? (
            <div className="editor-tree-loading">Empty workspace</div>
          ) : (
            renderEntries(ROOT_KEY, 0)
          )}
        </div>
      )}
    </div>
  );
}

export default FileTreePanel;