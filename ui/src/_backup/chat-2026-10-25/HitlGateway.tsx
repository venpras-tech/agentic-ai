import "./chat.css";

function HitlGateway({
  reason,
  onApprove,
  onDeny,
}: {
  reason: string;
  onApprove: () => void;
  onDeny: () => void;
}) {
  return (
    <div className="hitl-overlay" role="dialog" aria-modal="true" aria-label="Human-in-the-loop approval">
      <div className="hitl-modal">
        <div className="hitl-badge">HITL GATE</div>
        <h3 className="hitl-title">Agent requested approval</h3>
        <p className="hitl-reason">{reason || "The agent is waiting for your decision to continue."}</p>
        <div className="hitl-actions">
          <button type="button" className="hitl-approve" onClick={onApprove}>
            ✓ Approve Action
          </button>
          <button type="button" className="hitl-deny" onClick={onDeny}>
            ✕ Deny / Reroute
          </button>
        </div>
        <p className="hitl-note">The chat input is paused until you decide.</p>
      </div>
    </div>
  );
}

export default HitlGateway;