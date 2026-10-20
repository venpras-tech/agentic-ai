import { useShell } from "../context/AppContext";
import "./IndexStatusChip.css";

function IndexStatusChip() {
  const { projectPath, indexStatus, indexing, reindex } = useShell();
  if (!projectPath) return null;

  const label = indexing
    ? "Indexing…"
    : indexStatus
      ? `${indexStatus.indexedFiles.toLocaleString()} files · ${indexStatus.totalChars.toLocaleString()} chars`
      : "Index not built";

  return (
    <div className="index-chip">
      <button
        type="button"
        className="index-chip-btn"
        onClick={() => reindex()}
        disabled={indexing}
        title={
          indexStatus?.lastBuilt
            ? `Workspace index — last built ${new Date(indexStatus.lastBuilt).toLocaleString()}. Click to rebuild.`
            : "Build the workspace index"
        }
      >
        <span
          className={`index-chip-dot ${indexing ? "busy" : indexStatus ? "ready" : "idle"}`}
          aria-hidden="true"
        />
        <span className="index-chip-label">{label}</span>
      </button>
    </div>
  );
}

export default IndexStatusChip;
