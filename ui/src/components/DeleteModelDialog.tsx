import { useState } from "react";
import "./DeleteModelDialog.css";

export interface DeleteModelTarget {
  fileName: string;
  path: string;
  sizeBytes: number;
}

interface DeleteModelDialogProps {
  target: DeleteModelTarget | null;
  onConfirm: (target: DeleteModelTarget) => Promise<void>;
  onClose: () => void;
}

function formatBytes(bytes: number): string {
  if (!bytes) return "unknown size";
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(0)} MB`;
  if (bytes >= 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${bytes} B`;
}

/**
 * Confirmation for permanently removing a downloaded GGUF from disk.
 *
 * Deletion cannot be undone and the model has to be fetched again (multi-GB),
 * so the dialog spells out the file, its size, and that the removal is
 * permanent. The size is repeated in the confirm button because that is the
 * number people want to check before agreeing.
 */
function DeleteModelDialog({ target, onConfirm, onClose }: DeleteModelDialogProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  if (!target) return null;

  const run = async () => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await onConfirm(target);
      onClose();
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
      setBusy(false);
    }
  };

  return (
    <div
      className="del-dlg-backdrop"
      role="presentation"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget && !busy) onClose();
      }}
    >
      <div
        className="del-dlg"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="del-dlg-title"
        aria-describedby="del-dlg-desc"
        onKeyDown={(e) => {
          if (e.key === "Escape" && !busy) onClose();
        }}
      >
        <div className="del-dlg-icon">
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6" />
            <path d="M10 11v6M14 11v6" />
          </svg>
        </div>
        <div className="del-dlg-body">
          <h3 id="del-dlg-title" className="del-dlg-title">Delete this model?</h3>
          <p id="del-dlg-desc" className="del-dlg-desc">
            This permanently removes the file from disk. You will need to download it
            again to use it.
          </p>
          <div className="del-dlg-file" title={target.path}>
            {target.fileName}
          </div>
          <div className="del-dlg-meta">
            <span>Frees {formatBytes(target.sizeBytes)}</span>
          </div>
          {error ? <p className="del-dlg-error">{error}</p> : null}
        </div>
        <div className="del-dlg-actions">
          <button type="button" className="del-dlg-btn" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          <button type="button" className="del-dlg-btn danger" onClick={() => void run()} disabled={busy}>
            {busy ? "Deleting…" : "Delete"}
          </button>
        </div>
      </div>
    </div>
  );
}

export default DeleteModelDialog;
