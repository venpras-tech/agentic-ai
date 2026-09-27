import "./NeedsModelDialog.css";

function NeedsModelDialog({
  open,
  error,
  modelLabel,
  onProceed,
  onDismiss,
}: {
  open: boolean;
  error: string;
  modelLabel?: string;
  onProceed: () => void;
  onDismiss: () => void;
}) {
  if (!open) return null;

  return (
    <div className="model-prompt-overlay" role="dialog" aria-modal="true" aria-label="Model required">
      <div className="model-prompt-modal">
        <div className="model-prompt-badge">MODEL REQUIRED</div>
        <h3 className="model-prompt-title">Your last model isn't available</h3>
        <p className="model-prompt-model">{modelLabel || "AI Editor"} could not be reconnected.</p>
        {error && <p className="model-prompt-reason">{error}</p>}
        <p className="model-prompt-hint">
          Download a local model or connect a provider to start working. The app stays fully functional —
          chat and tools simply wait for a model.
        </p>
        <div className="model-prompt-actions">
          <button type="button" className="model-prompt-proceed" onClick={onProceed}>
            Choose a model →
          </button>
          <button type="button" className="model-prompt-dismiss" onClick={onDismiss}>
            Later
          </button>
        </div>
      </div>
    </div>
  );
}

export default NeedsModelDialog;