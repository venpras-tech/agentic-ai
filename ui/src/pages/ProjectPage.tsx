import { useState, useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useShell, useContextPane } from "../context/AppContext";
import { api, isTauriRuntime, type IndexSearchHit } from "../lib/ipc";
import "./ProjectPage.css";

interface FileTreeEntry {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileTreeEntry[];
}

function ProjectPage() {
  const { projectPath, setProjectPath, indexStatus, indexing, reindex } = useShell();
  const { addContextPath } = useContextPane();
  const [files, setFiles] = useState<FileTreeEntry[]>([]);
  const [selectedFile, setSelectedFile] = useState<FileTreeEntry | null>(null);
  const [fileContent, setFileContent] = useState("");
  const [loading, setLoading] = useState(false);
  const [query, setQuery] = useState("");
  const [searchResults, setSearchResults] = useState<IndexSearchHit[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [semantic, setSemantic] = useState(false);
  const [indexError, setIndexError] = useState("");

  useEffect(() => {
    if (projectPath) {
      void loadFileTree();
    }
  }, [projectPath]);

  useEffect(() => {
    if (!projectPath || !isTauriRuntime()) return;
    const un = listen("workspace://file-changed", () => {
      void loadFileTree();
    });
    return () => {
      un.then((fn) => fn());
    };
  }, [projectPath]);

  const runSearch = async () => {
    if (!projectPath || !query.trim()) return;
    setSearching(true);
    setIndexError("");
    try {
      const hits = semantic
        ? await api.indexSemanticSearch(projectPath, query.trim())
        : await api.indexSearch(projectPath, query.trim());
      setSearchResults(hits);
    } catch (err) {
      setIndexError(err instanceof Error ? err.message : String(err));
    } finally {
      setSearching(false);
    }
  };

  const rebuildIndex = async () => {
    if (!projectPath) return;
    setIndexError("");
    reindex();
    if (query.trim()) await runSearch();
  };

  const openHit = (hit: IndexSearchHit) => {
    if (!projectPath) return;
    const base = projectPath.replaceAll("\\", "/").replace(/\/?$/, "/");
    const target = `${base}${hit.path}`;
    window.dispatchEvent(new CustomEvent<string>("vencoder-open-editor", { detail: target }));
  };

  const loadFileTree = async () => {
    if (!projectPath) return;
    setLoading(true);
    try {
      const entries = await api.listDirectory(projectPath);
      const tree = await buildTree(projectPath, entries);
      setFiles([{ name: projectPath.split(/[/\\]/).pop() || projectPath, path: projectPath, isDir: true, children: tree }]);
    } catch {
      console.error("Failed to load file tree");
    }
    setLoading(false);
  };

  const buildTree = async (root: string, entries: Array<{ name: string; path: string; isDir: boolean }>): Promise<FileTreeEntry[]> => {
    const result: FileTreeEntry[] = [];
    for (const entry of entries) {
      if (entry.name === "node_modules" || entry.name === ".git" || entry.name === "target" || entry.name === "dist") {
        continue;
      }
      const node: FileTreeEntry = { ...entry };
      if (entry.isDir) {
        try {
          const children = await api.listDirectory(root, entry.path);
          node.children = await buildTree(root, children);
        } catch {
          node.children = [];
        }
      }
      result.push(node);
    }
    result.sort((a, b) => (a.isDir === b.isDir ? a.name.localeCompare(b.name) : a.isDir ? -1 : 1));
    return result;
  };

  const handleBrowseProject = async () => {
    try {
      const path = await api.pickWorkspaceFolder();
      if (path) {
        setProjectPath(path);
        await api.agentSetWorkspace(path);
      }
    } catch {
      console.error("Failed to pick workspace");
    }
  };

  const handleFileClick = async (file: FileTreeEntry) => {
    if (file.isDir) return;
    setSelectedFile(file);
    try {
      const result = await api.readTextFile(file.path);
      setFileContent(result.content);
    } catch {
      console.error("Failed to read file");
    }
  };

  const handleAddToContext = (file: FileTreeEntry) => {
    if (file.isDir) return;
    const rel = projectPath ? file.path.replace(projectPath.replaceAll("\\", "/") + "/", "") : file.path;
    addContextPath(rel);
  };

  return (
    <div className="project-view">
      <div className="project-header">
        <div className="project-header-row">
          <label className="project-label">Workspace</label>
          <button
            type="button"
            className="project-path project-path-full"
            onClick={() => void handleBrowseProject()}
            title="Click to change project folder"
          >
            <span className="project-path-icon" aria-hidden="true" />
            <span className="project-path-text">
              {projectPath || "No folder selected"}
            </span>
          </button>
        </div>
        <div className="project-header-row project-header-row-buttons">
          <button type="button" className="btn-index" onClick={() => void loadFileTree()} disabled={!projectPath}>
            Refresh
          </button>
        </div>
      </div>
      <div className="project-index">
        <div className="project-index-row">
          <input
            className="project-index-input"
            value={query}
            placeholder="Search workspace (regex or plain text)…"
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void runSearch();
            }}
            disabled={!projectPath}
          />
          <button type="button" className="btn-index" onClick={() => void runSearch()} disabled={!projectPath || searching}>
            {searching ? "…" : "Search"}
          </button>
          <label className="project-index-semantic">
            <input type="checkbox" checked={semantic} onChange={(e) => setSemantic(e.target.checked)} />
            <span>Semantic</span>
          </label>
          <button type="button" className="btn-index" onClick={() => void rebuildIndex()} disabled={!projectPath || indexing}>
            {indexing ? "Building…" : "Rebuild index"}
          </button>
        </div>
        <div className="project-index-status">
          {indexStatus
            ? `${indexStatus.indexedFiles.toLocaleString()} files · ${indexStatus.totalChars.toLocaleString()} chars${indexStatus.lastBuilt ? ` · built ${new Date(indexStatus.lastBuilt).toLocaleTimeString()}` : ""}`
            : "Index not built yet — search the workspace to build it lazily."}
        </div>
        {indexError && <div className="project-index-error">{indexError}</div>}
        {searchResults && (
          <div className="project-index-results">
            {searchResults.length === 0 ? (
              <div className="project-index-empty">No matches.</div>
            ) : (
              searchResults.map((h, i) => (
                <button
                  key={i}
                  type="button"
                  className="project-index-hit"
                  onClick={() => openHit(h)}
                  title={`Open ${h.path}:${h.lineNumber}`}
                >
                  <span className="project-index-hit-path">
                    {h.path}:{h.lineNumber}
                  </span>
                  <span className="project-index-hit-line">{h.line}</span>
                </button>
              ))
            )}
          </div>
        )}
      </div>
      <div className="project-explorer">
        <div className="project-tree-panel">
          <div className="project-tree-header">Files</div>
          <div className="project-tree">
            {loading ? (
              <div className="project-tree-loading">Loading...</div>
            ) : files.length === 0 ? (
              <div className="project-tree-loading">No files</div>
            ) : (
              <FileTree files={files} onFileClick={handleFileClick} selectedFile={selectedFile} onAddContext={handleAddToContext} />
            )}
          </div>
        </div>
        <div className="project-editor-panel">
          {selectedFile ? (
            <div className="project-editor-container">
              <div className="project-editor-header">
                <span className="project-editor-filename">{selectedFile.name}</span>
                <button type="button" className="btn-index" onClick={() => handleAddToContext(selectedFile)}>
                  + Context
                </button>
              </div>
              <div className="project-editor-scroll">
                <pre className="project-editor-code">
                  <code>{fileContent}</code>
                </pre>
              </div>
            </div>
          ) : (
            <div className="project-editor-placeholder">Select a file to view</div>
          )}
        </div>
      </div>
    </div>
  );
}

interface FileTreeProps {
  files: FileTreeEntry[];
  onFileClick: (file: FileTreeEntry) => void;
  selectedFile: FileTreeEntry | null;
  onAddContext: (file: FileTreeEntry) => void;
  depth?: number;
}

function FileTree({ files, onFileClick, selectedFile, onAddContext, depth = 0 }: FileTreeProps) {
  return (
    <div className="project-tree-children" style={{ paddingLeft: depth > 0 ? 12 : 0 }}>
      {files.map((file) => (
        <div key={file.path}>
          <div
            className={`project-tree-item ${selectedFile?.path === file.path ? "selected" : ""}`}
            onClick={() => onFileClick(file)}
            onContextMenu={(e) => {
              e.preventDefault();
              onAddContext(file);
            }}
            title={file.isDir ? file.name : `${file.name} — right-click to add to context`}
          >
            <span className="project-tree-item-icon">
              {file.isDir ? "📁" : "📄"}
            </span>
            <span className="project-tree-item-name">{file.name}</span>
          </div>
          {file.isDir && file.children && (
            <FileTree
              files={file.children}
              onFileClick={onFileClick}
              selectedFile={selectedFile}
              onAddContext={onAddContext}
              depth={depth + 1}
            />
          )}
        </div>
      ))}
    </div>
  );
}

export default ProjectPage;