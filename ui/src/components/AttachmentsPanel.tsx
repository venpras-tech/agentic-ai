import { useCallback, useEffect, useState } from "react";
import { api, type AttachedFileInfo } from "../lib/ipc";
import "./AttachmentsPanel.css";

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} kB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

/**
 * Session RAG attachments: the files the agent can search via
 * `search_attached_files`. Lists path/size/chunk count, supports attaching
 * via the native file picker and detaching (which clears the backend index
 * entry). Mounted in the composer so it is reachable from both the Chat and
 * Workspace routes.
 */
function AttachmentsPanel() {
  const [files, setFiles] = useState<AttachedFileInfo[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setFiles((await api.agentListAttachments()) ?? []);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const attach = async () => {
    setBusy(true);
    try {
      const path = await api.pickTextFile();
      if (path) {
        await api.agentAttachFile(path);
        await refresh();
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const detach = async (path: string) => {
    try {
      await api.agentDetachFile(path);
      await refresh();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <div className="attachments-panel" aria-label="File attachments">
      <div className="attachments-header">
        <span className="attachments-title">
          Attachments{files.length > 0 ? ` (${files.length})` : ""}
        </span>
        <button
          type="button"
          className="attachments-add"
          onClick={() => void attach()}
          disabled={busy}
          title="Attach a text file to the session index"
        >
          {busy ? "…" : "+ File"}
        </button>
      </div>
      {error && <div className="attachments-error">{error}</div>}
      {files.length > 0 && (
        <ul className="attachments-list">
          {files.map((f) => (
            <li key={f.path} className="attachments-row">
              <span className="attachments-path" title={f.path}>
                {f.path.split(/[/\\]/).pop()}
              </span>
              <span className="attachments-meta">
                {formatBytes(f.bytes)} · {f.chunkCount} chunk
                {f.chunkCount === 1 ? "" : "s"}
              </span>
              <button
                type="button"
                className="attachments-detach"
                onClick={() => void detach(f.path)}
                title={`Detach ${f.path}`}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

export default AttachmentsPanel;
