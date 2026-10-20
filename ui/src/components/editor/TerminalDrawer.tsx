import { useCallback, useEffect, useRef, useState } from "react";
import { api, subscribeTerminalOutput, type TerminalOutputEvent } from "../../lib/ipc";
import "./TerminalDrawer.css";

interface TerminalDrawerProps {
  projectPath: string | null;
  open: boolean;
  onToggle: () => void;
}

function TerminalDrawer({ projectPath, open, onToggle }: TerminalDrawerProps) {
  const [id, setId] = useState<string | null>(null);
  const [lines, setLines] = useState<TerminalOutputEvent[]>([]);
  const [input, setInput] = useState("");
  const [exited, setExited] = useState(false);
  const [error, setError] = useState("");
  const [selection, setSelection] = useState("");
  const scrollRef = useRef<HTMLDivElement | null>(null);

  const append = useCallback((evt: TerminalOutputEvent) => {
    setLines((prev) => [...prev, evt]);
  }, []);

  const clearSession = useCallback(() => {
    setLines([]);
    setInput("");
    setExited(false);
    setError("");
    setId(null);
  }, []);

  const spawn = useCallback(
    async (cwd: string | null) => {
      setError("");
      try {
        const newId = await api.terminalSpawn(cwd ?? undefined);
        setLines((prev) => [...prev, { id: newId, data: `$ ${cwd ?? "<workspace>"}\n`, stream: "stdout" }]);
        setExited(false);
        setId(newId);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    []
  );

  useEffect(() => {
    if (!open) return;
    let unsub: (() => void) | undefined;
    let disposed = false;
    void subscribeTerminalOutput((evt) => {
      if (evt.id !== id) return;
      if (evt.stream === "exit") {
        setExited(true);
        append(evt);
        return;
      }
      append(evt);
    }).then((fn) => {
      if (disposed) fn();
      else unsub = fn;
    });
    return () => {
      disposed = true;
      unsub?.();
    };
  }, [open, id, append]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  // Kill the session when the drawer unmounts (page leave).
  useEffect(() => {
    return () => {
      const live = id;
      if (live) void api.terminalKill(live).catch(() => {});
    };
  }, [id]);

  const send = () => {
    const line = input;
    if (!line || !id || exited) return;
    setInput("");
    void api.terminalWrite(id, line).catch((err) => {
      setError(err instanceof Error ? err.message : String(err));
    });
    append({ id, data: line + "\n", stream: "stdout" });
  };

  const close = () => {
    if (id) void api.terminalKill(id).catch(() => {});
    clearSession();
    onToggle();
  };

  // Capture a text selection in the output so it can be @-mentioned into the
  // chat composer (push any terminal tail into the prompt as context).
  const captureSelection = () => {
    const sel = window.getSelection();
    setSelection(sel ? sel.toString() : "");
  };

  const mentionSelection = () => {
    const text = selection.trim();
    if (!text) return;
    const block = `@terminal\n\`\`\`\n${text}\n\`\`\`\n`;
    window.dispatchEvent(new CustomEvent("vencoder-insert-composer", { detail: block }));
    window.getSelection()?.removeAllRanges();
    setSelection("");
  };

  if (!open) {
    return (
      <div className="terminal-drawer-closed">
        <button type="button" className="terminal-tab" onClick={onToggle} title="Open interactive terminal">
          Terminal
        </button>
      </div>
    );
  }

  return (
    <div className="terminal-drawer">
      <div className="terminal-header">
        <span className="terminal-title">Terminal {id ? "— live" : exited ? "— ended" : ""}</span>
        <div className="terminal-actions">
          <button type="button" className="btn-index" onClick={() => void spawn(projectPath)} disabled={!!id && !exited}>
            Spawn
          </button>
          <button
            type="button"
            className="btn-index"
            onClick={mentionSelection}
            disabled={!selection.trim()}
            title="Insert the selected output into the chat composer as @terminal"
          >
            Mention
          </button>
          <button type="button" className="btn-index" onClick={close} title="Close terminal">
            Close
          </button>
        </div>
      </div>
      {error && <div className="terminal-error">{error}</div>}
      <div className="terminal-output" ref={scrollRef} onMouseUp={captureSelection} onKeyUp={captureSelection}>
        {lines.length === 0 && <span className="terminal-empty">Press Spawn to start a shell in the workspace.</span>}
        {lines.map((e, i) => (
          <div key={i} className={`terminal-line terminal-${e.stream}`}>
            {e.stream === "exit"
              ? `[process exited with code ${e.exitCode ?? "?"}]`
              : e.data}
          </div>
        ))}
      </div>
      <div className="terminal-input-row">
        <input
          className="terminal-input"
          value={input}
          placeholder={exited || !id ? "Session inactive — press Spawn" : "Type a command…"}
          disabled={exited || !id}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") send();
          }}
          spellCheck={false}
        />
        <button type="button" className="btn-index" onClick={send} disabled={!input || exited || !id}>
          Run
        </button>
      </div>
    </div>
  );
}

export default TerminalDrawer;