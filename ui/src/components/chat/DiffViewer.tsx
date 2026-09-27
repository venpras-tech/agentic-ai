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

function DiffViewer({ path, content }: { path?: string; content: string }) {
  const lines = parseUnifiedDiff(content);
  const adds = lines.filter((l) => l.type === "add").length;
  const dels = lines.filter((l) => l.type === "del").length;

  return (
    <div className="chat-diff">
      <div className="chat-diff-head">
        <span className="chat-diff-path">{path || "diff"}</span>
        <span className="chat-diff-stats">
          <span className="chat-diff-add">+{adds}</span>
          <span className="chat-diff-del">−{dels}</span>
        </span>
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