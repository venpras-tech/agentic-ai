import { useEffect, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import "./chat.css";

function ReasoningAccordion({ text, streaming }: { text: string; streaming?: boolean }) {
  const [open, setOpen] = useState(false);
  const running = Boolean(streaming);

  useEffect(() => {
    if (!running) setOpen(false);
  }, [running]);

  const preview = running && !text ? "Inspecting …" : text.slice(0, 90);

  return (
    <div className={`chat-reasoning${open ? " open" : ""}${running ? " running" : ""}`}>
      <button
        type="button"
        className="chat-reasoning-head"
        onClick={() => setOpen((o) => !o)}
        title={open ? "Collapse reasoning" : "Expand reasoning"}
      >
        <span className={`chat-reasoning-spinner ${running ? "spin" : ""}`} aria-hidden="true" />
        <span className="chat-reasoning-label">{running ? "agent_thought · reasoning" : "agent_thought"}</span>
        <span className="chat-reasoning-preview">{preview}</span>
        <span className="chat-reasoning-chevron">{open ? "▾" : "▸"}</span>
      </button>
      {(open || running) && (
        <div className="chat-reasoning-body">
          {text ? (
            <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
          ) : (
            <span className="chat-reasoning-placeholder">Thinking…</span>
          )}
          {running && <span className="msg-caret" />}
        </div>
      )}
    </div>
  );
}

export default ReasoningAccordion;