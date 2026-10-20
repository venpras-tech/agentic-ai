import { useState } from "react";
import "./chat.css";

export type ToolStatus = "pending" | "running" | "success" | "error" | "failed";

function statusClass(status: ToolStatus): string {
  switch (status) {
    case "pending":
    case "running":
      return "pending";
    case "error":
    case "failed":
      return "error";
    default:
      return "success";
  }
}

function statusLabel(status: ToolStatus): string {
  switch (status) {
    case "pending":
      return "pending";
    case "running":
      return "running…";
    case "error":
    case "failed":
      return "error";
    default:
      return "success";
  }
}

function statusGlyph(status: ToolStatus, open: boolean) {
  if (status === "pending" || status === "running") {
    return <span className="tool-badge-spinner" aria-hidden="true" />;
  }
  if (status === "error" || status === "failed") {
    return (
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" aria-hidden="true">
        <path d="M3 17 12 4l9 13H3Z" />
      </svg>
    );
  }
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3" aria-hidden="true">
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );
}

function ToolExecutionBadge({
  name,
  status,
  output,
  streaming,
  durationMs,
  exitCode,
  args,
}: {
  name: string;
  status: ToolStatus;
  output: string;
  streaming?: boolean;
  durationMs?: number;
  exitCode?: number;
  args?: string;
}) {
  const live = (status === "pending" || status === "running" || Boolean(streaming)) && Boolean(output);
  const [open, setOpen] = useState(live);
  const [showArgs, setShowArgs] = useState(false);
  const cls = statusClass(status);
  const hasOutput = output.trim().length > 0;
  const hasArgs = Boolean(args && args.trim());

  return (
    <div className={`chat-tool tool-${cls}${open ? " open" : ""}`}>
      <button
        type="button"
        className="chat-tool-head"
        onClick={() => setOpen((o) => !o)}
        title={open ? "Collapse output" : "Expand output"}
      >
        <span className="chat-tool-glyph">{statusGlyph(status, open)}</span>
        <span className="chat-tool-name">{name.replaceAll("_", " ")}</span>
        {hasArgs && (
          <span
            className="chat-tool-args-toggle"
            role="button"
            tabIndex={0}
            onClick={(e) => {
              e.stopPropagation();
              setShowArgs((v) => !v);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                e.stopPropagation();
                setShowArgs((v) => !v);
              }
            }}
            title={showArgs ? "Hide arguments" : "Show arguments"}
          >
            args
          </span>
        )}
        {exitCode != null && (
          <span className={`chat-tool-exit${exitCode === 0 ? " ok" : " err"}`} title={`Exit code ${exitCode}`}>
            exit {exitCode}
          </span>
        )}
        {durationMs != null && durationMs >= 0 && (
          <span className="chat-tool-duration" title="Duration">
            {(durationMs / 1000).toFixed(1)}s
          </span>
        )}
        <span className="chat-tool-status">{statusLabel(status)}</span>
        <span className="chat-tool-chevron">{open ? "▾" : "▸"}</span>
      </button>
      {showArgs && hasArgs && (
        <div className="chat-tool-args">
          <pre className="chat-tool-args-pre">{args}</pre>
        </div>
      )}
      {open && hasOutput && (
        <div className="chat-tool-terminal">
          <div className="chat-tool-terminal-title">stdout / stderr</div>
          <pre className="chat-tool-output">{output}</pre>
        </div>
      )}
    </div>
  );
}

export default ToolExecutionBadge;