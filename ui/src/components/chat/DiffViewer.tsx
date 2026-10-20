import { useState } from "react";
import { useAgentStreamContext } from "../../context/AgentStreamContext";
import "./chat.css";

export function parseUnifiedDiff(text: string) {
  const lines: Array<{ type: "meta" | "add" | "del" | "hunk" | "context"; text: string }> = [];
  for (const raw of text.split("\n")) {
    const line = raw.replace(/\r$/, "");
    if (/^@@ /.test(line)) lines.push({ type: "hunk", text: line });
    else if (/^\+/.test(line) && !/^\+\+\+/.test(line)) lines.push({ type: "add", text: line });
    else if (/^-/.test(line) && !/^---/.test(line)) lines.push({ type: "del", text: line });
    else if (/^(---|\+\+\+)/.test(line)) lines.push({ type: "meta", text: line });
    else lines.push({ type: "context", text: line });
  }
  return lines;
}

function DiffViewer({
  path,
  content,
  status,
  openPath,
}: {
  path?: string;
  content: string;
  /** pending/applied/rejected/failed/conflict — rendered as a header chip. */
  status?: string;
  /** Absolute path emitted on `vencoder-open-editor` when shown in chat. */
  openPath?: string;
}) {
  const lines = parseUnifiedDiff(content);
  const adds = lines.filter((l) => l.type === "add").length;
  const dels = lines.filter((l) => l.type === "del").length;

  const { pendingEdits, applyEditGroup, rejectEditGroup } = useAgentStreamContext();
  const [busy, setBusy] = useState(false);

  // Pending edits for this exact path across every open group, so a transcript
  // diff can be accepted/rejected inline without opening the review pane.
  const rel = (path || "").replaceAll("\\", "/");
  const matches = pendingEdits.flatMap((g) =>
    g.edits
      .filter((e) => e.path.replaceAll("\\", "/") === rel && e.status === "pending")
      .map((e) => ({ groupId: g.id, editId: e.id })),
  );

  const runFileAction = async (action: "apply" | "reject") => {
    if (matches.length === 0 || busy) return;
    setBusy(true);
    try {
      const byGroup = new Map<string, string[]>();
      for (const m of matches) {
        const list = byGroup.get(m.groupId) ?? [];
        list.push(m.editId);
        byGroup.set(m.groupId, list);
      }
      for (const [groupId, editIds] of byGroup) {
        if (action === "apply") await applyEditGroup(groupId, editIds);
        else await rejectEditGroup(groupId, editIds);
      }
    } finally {
      setBusy(false);
    }
  };

  const openEditor = () => {
    if (openPath) {
      window.dispatchEvent(new CustomEvent("vencoder-open-editor", { detail: openPath }));
    }
  };

  return (
    <div className="chat-diff">
      <div className="chat-diff-head">
        <span className="chat-diff-path">{path || "diff"}</span>
        {status && (
          <span className={`chat-diff-status edit-status--${status}`} title={`Edit status: ${status}`}>
            {status}
          </span>
        )}
        <span className="chat-diff-stats">
          <span className="chat-diff-add">+{adds}</span>
          <span className="chat-diff-del">−{dels}</span>
        </span>
        {matches.length > 0 && (
          <span className="chat-diff-actions">
            <button
              type="button"
              className="chat-diff-accept"
              disabled={busy}
              onClick={() => void runFileAction("apply")}
              title="Accept this file's changes"
            >
              accept
            </button>
            <button
              type="button"
              className="chat-diff-reject"
              disabled={busy}
              onClick={() => void runFileAction("reject")}
              title="Reject this file's changes"
            >
              reject
            </button>
          </span>
        )}
        {openPath && (
          <button type="button" className="chat-diff-open" onClick={openEditor} title="Open in editor">
            open
          </button>
        )}
      </div>
      <div className="chat-diff-body">
        {lines.map((l, i) => (
          <div key={i} className={`chat-diff-line diff-${l.type}`}>
            <span className="chat-diff-line-sign">{l.text.slice(0, 1)}</span>
            <span className="chat-diff-line-text">{l.text.slice(1)}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export default DiffViewer;