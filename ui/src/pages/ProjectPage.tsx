import { useState, useEffect } from "react";
import { useApp } from "../context/AppContext";
import { api } from "../lib/ipc";
import "./ProjectPage.css";

interface FileTreeEntry {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileTreeEntry[];
}

function ProjectPage() {
  const { projectPath, setProjectPath, addContextPath } = useApp();
  const [files, setFiles] = useState<FileTreeEntry[]>([]);
  const [selectedFile, setSelectedFile] = useState<FileTreeEntry | null>(null);
  const [fileContent, setFileContent] = useState("");
  const [loading, setLoading] = useState(false);
  const [showTemplateDropdown, setShowTemplateDropdown] = useState(false);

  useEffect(() => {
    if (projectPath) {
      void loadFileTree();
    }
  }, [projectPath]);

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
          <div className="project-template-wrap">
            <button
              type="button"
              className="btn-index"
              onClick={() => setShowTemplateDropdown(!showTemplateDropdown)}
              disabled={!projectPath}
            >
              + Project template
            </button>
            {showTemplateDropdown && (
              <div className="project-template-dropdown">
                <button type="button" className="project-template-opt">Python</button>
                <button type="button" className="project-template-opt">React</button>
                <button type="button" className="project-template-opt">Node.js</button>
                <button type="button" className="project-template-opt">Rust</button>
              </div>
            )}
          </div>
        </div>
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