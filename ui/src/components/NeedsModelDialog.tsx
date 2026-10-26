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
        <div className="model-prompt-badge">AGENT REQUIRED</div>
        <h3 className="model-prompt-title">No agent backend is connected</h3>
        <p className="model-prompt-model">{modelLabel || "Vencoder"} could not be reconnected.</p>
        {error && <p className="model-prompt-reason">{error}</p>}
        <p className="model-prompt-hint">
          Connect an external agent (OpenAI, Claude, Ollama, …) or load a local GGUF model. Chat and
          tools stay idle until a backend is active.
        </p>
        <div className="model-prompt-actions">
          <button type="button" className="model-prompt-proceed" onClick={onProceed}>
            Connect agent →
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