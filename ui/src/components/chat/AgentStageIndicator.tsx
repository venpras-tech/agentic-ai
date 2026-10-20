import { useEffect, useState } from "react";
import { isStageVisible, type AgentStageState } from "../../lib/agentStages";

/**
 * The "the model is working on it" row rendered under the latest message.
 *
 * Without it a request looks dead from the moment it is sent until the first
 * token lands, which on a local model can be many seconds — the spinner, the
 * current stage name, and a live elapsed timer are the only signal that
 * anything is happening. Prompt evaluation on a CPU-only model is routinely the
 * longest phase of a turn, so the timer plus the prompt size make that wait
 * legible instead of looking like a hang.
 */
export default function AgentStageIndicator({ stage }: { stage: AgentStageState }) {
  const visible = Boolean(stage.label) && isStageVisible(stage);
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (!visible || !stage.since) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [visible, stage.since]);

  if (!visible) return null;
  const elapsedSecs = stage.since ? Math.max(0, Math.floor((now - stage.since) / 1000)) : 0;

  return (
    <div className="agent-stage" role="status" aria-live="polite">
      <span className="agent-stage-dots" aria-hidden>
        <i />
        <i />
        <i />
      </span>
      <span className="agent-stage-label">{stage.label}</span>
      {stage.promptTokens ? (
        <span className="agent-stage-prompt">{stage.promptTokens.toLocaleString()} tok prompt</span>
      ) : null}
      {stage.steps > 0 && <span className="agent-stage-steps">step {stage.steps}</span>}
      {elapsedSecs > 0 && <span className="agent-stage-elapsed">{elapsedSecs}s</span>}
    </div>
  );
}
