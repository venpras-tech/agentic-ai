import { useState } from "react";
import "./CancelDownloadDialog.css";

interface CancelDownloadDialogProps {
  open: boolean;
  repoId: string;
  file: string;
  onCancel: (repoId: string, file: string, deleteFile: boolean) => Promise<void>;
  onClose: () => void;
}

/**
 * Confirmation shown before a download is cancelled, so an accidental press of
 * the ✕ button doesn't silently kill a multi-GB download.
 *
 * "Cancel download" keeps the `*.part` file so a later resume continues from
 * the same byte offset. "Cancel & delete file" removes the partial file and
 * any later attempt re-downloads from scratch.
 */
function CancelDownloadDialog({ open, repoId, file, onCancel, onClose }: CancelDownloadDialogProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");

  if (!open) return null;

  const run = async (deleteFile: boolean) => {
    if (busy) return;
    setBusy(true);
    setError("");
    try {
      await onCancel(repoId, file, deleteFile);
      onClose();
    } catch (err) {
      setError(String(err instanceof Error ? err.message : err));
      setBusy(false);
    }
  };

  return (
    <div className="cancel-dlg-backdrop" role="presentation" onMouseDown={(e) => { if (e.target === e.currentTarget) onClose(); }}>
      <div className="cancel-dlg" role="alertdialog" aria-modal="true" aria-labelledby="cancel-dlg-title">
        <div className="cancel-dlg-icon">
          <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="10" />
            <path d="M12 8v4M12 16h.01" />
          </svg>
        </div>
        <div className="cancel-dlg-body">
          <h3 id="cancel-dlg-title" className="cancel-dlg-title">Cancel download?</h3>
          <p className="cancel-dlg-file" title={`${repoId} / ${file}`}>{file}</p>
          <div className="cancel-dlg-options">
            <div className="cancel-dlg-option">
              <strong>Cancel download</strong>
              <span>Keeps the partial file — you can resume later from where it stopped.</span>
            </div>
            <div className="cancel-dlg-option">
              <strong>Cancel &amp; delete file</strong>
              <span>Removes the partial file. Restarting downloads it from scratch.</span>
            </div>
          </div>
          {error ? <p className="cancel-dlg-error">{error}</p> : null}
        </div>
        <div className="cancel-dlg-actions">
          <button type="button" className="cancel-dlg-btn" onClick={() => onClose()} disabled={busy}>
            Keep downloading
          </button>
          <button type="button" className="cancel-dlg-btn secondary" onClick={() => void run(false)} disabled={busy}>
            {busy ? "Cancelling…" : "Cancel download"}
          </button>
          <button type="button" className="cancel-dlg-btn danger" onClick={() => void run(true)} disabled={busy}>
            Cancel &amp; delete file
          </button>
        </div>
      </div>
    </div>
  );
}

export default CancelDownloadDialog;