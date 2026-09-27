import { useState } from "react";
import type { PermissionDecision, PermissionRequest } from "../../lib/ipc";
import type { QuestionRequest } from "../../hooks/useAgentStream";
import "./PermissionPrompt.css";

interface PermissionPromptProps {
  request: PermissionRequest;
  onDecide: (decision: PermissionDecision) => void;
}

interface QuestionPromptProps {
  request: QuestionRequest;
  onAnswer: (answer: string) => void;
}

/**
 * Blocking approval for a policy `ask` verdict. Mirrors the backend's four
 * accepted decisions, including "always" which persists an allow rule.
 */
export function PermissionPrompt({ request, onDecide }: PermissionPromptProps) {
  return (
    <div className="perm-prompt" role="alertdialog" aria-label="Approval required">
      <div className="perm-head">
        <span className="perm-tool">{request.tool}</span>
        <span className="perm-title">needs your approval</span>
      </div>
      <pre className="perm-summary">{request.summary}</pre>
      {request.review && <p className="perm-review">Review: {request.review}</p>}
      <div className="perm-actions">
        <button type="button" className="perm-btn perm-allow" onClick={() => onDecide("allow_once")}>
          Allow once
        </button>
        <button
          type="button"
          className="perm-btn"
          onClick={() => onDecide("allow_session")}
        >
          Allow this session
        </button>
        <button type="button" className="perm-btn" onClick={() => onDecide("always_allow")}>
          Always allow
        </button>
        <button type="button" className="perm-btn perm-deny" onClick={() => onDecide("deny")}>
          Deny
        </button>
      </div>
    </div>
  );
}

/** Blocking `ask_question` from the agent: preset choices plus free text. */
export function QuestionPrompt({ request, onAnswer }: QuestionPromptProps) {
  const [text, setText] = useState("");

  const submit = () => {
    const answer = text.trim();
    if (answer) onAnswer(answer);
  };

  return (
    <div className="perm-prompt" role="alertdialog" aria-label="The agent has a question">
      <div className="perm-head">
        <span className="perm-title">The agent is asking</span>
      </div>
      <p className="perm-summary">{request.question}</p>
      {request.choices.length > 0 && (
        <div className="perm-actions">
          {request.choices.map((choice) => (
            <button
              key={choice}
              type="button"
              className="perm-btn"
              onClick={() => onAnswer(choice)}
            >
              {choice}
            </button>
          ))}
        </div>
      )}
      <div className="perm-actions">
        <input
          className="perm-input"
          value={text}
          placeholder="Type an answer…"
          aria-label="Answer the agent"
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
        />
        <button type="button" className="perm-btn perm-allow" onClick={submit} disabled={!text.trim()}>
          Send
        </button>
      </div>
    </div>
  );
}

export default PermissionPrompt;
