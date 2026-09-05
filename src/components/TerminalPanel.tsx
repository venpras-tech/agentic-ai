import { useEffect, useRef, useState } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import "@xterm/xterm/css/xterm.css";

import {
  api,
  subscribeTerminalOutput,
  type TerminalOutputEvent,
} from "../lib/ipc";

interface TerminalPanelProps {
  visible: boolean;
  /** Draggable height in px (mirrors ConsolePanel). */
  height?: number;
  /** Optional working directory for the spawned shell. */
  cwd?: string | null;
}

/** Integrated interactive terminal pane backed by a persistent shell session. */
export default function TerminalPanel({ visible, height, cwd }: TerminalPanelProps) {
  const hostRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const idRef = useRef<string | null>(null);
  const [ready, setReady] = useState(false);
  const [label, setLabel] = useState("Shell");

  // Mount xterm once (kept alive across visibility toggles).
  useEffect(() => {
    if (!hostRef.current || termRef.current) return;

    const q = {
      fontFamily: "'Cascadia Mono', 'JetBrains Mono', Consolas, monospace",
      fontSize: 12,
      lineHeight: 1.25,
      theme: {
        background: "#0b0e14",
        foreground: "#d4d4d4",
        cursor: "#9cdcfe",
        selectionBackground: "#264f78",
      },
      cursorBlink: true,
      scrollback: 3000,
      convertEol: true,
    };
    const term = new Terminal(q);
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(hostRef.current);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;
    term.writeln("Interactive terminal — type a command below and press Enter.\r\n");
    setReady(true);

    return () => {
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
      const id = idRef.current;
      if (id) void api.terminalKill(id).catch(() => {});
      idRef.current = null;
    };
  }, []);

  // Fit when the panel is shown or the container resizes.
  useEffect(() => {
    if (!visible || !fitRef.current) return;
    const t = window.setTimeout(() => fitRef.current?.fit(), 30);
    const ro = new ResizeObserver(() => fitRef.current?.fit());
    if (hostRef.current) ro.observe(hostRef.current);
    return () => {
      window.clearTimeout(t);
      ro.disconnect();
    };
  }, [visible, height]);

  // Spawn shell + stream output.
  useEffect(() => {
    if (!visible || idRef.current) return;
    let disposed = false;
    api
      .terminalSpawn(cwd ?? undefined)
      .then((id) => {
        if (disposed) {
          void api.terminalKill(id).catch(() => {});
          return;
        }
        idRef.current = id;
        setLabel("Shell");
      })
      .catch((err) => {
        termRef.current?.writeln(`\r\n[terminal] failed to start shell: ${err}`);
      });

    const unsub = subscribeTerminalOutput((e: TerminalOutputEvent) => {
      if (disposed) return;
      const term = termRef.current;
      if (!term) return;
      if (e.stream === "exit") {
        term.writeln(`\r\n[process exited · code ${e.exitCode ?? 0}]`);
        idRef.current = null;
        setLabel("Shell (exited)");
        return;
      }
      // Only render output for the active terminal.
      if (idRef.current && e.id === idRef.current) {
        term.write(e.data);
      }
    });

    return () => {
      disposed = true;
      unsub.then((fn) => fn());
    };
  }, [visible, cwd]);

  const handleSubmit = (raw: string) => {
    const id = idRef.current;
    if (!id) return;
    const term = termRef.current!;
    const cmd = raw.trimEnd();
    if (!cmd) {
      term.writeln("");
      return;
    }
    // Local echo so the command is visible regardless of backend echo.
    term.writeln(`${label}$ ${cmd}`);
    void api
      .terminalWrite(id, cmd)
      .catch((err) => term.writeln(`\r\n[terminal] ${err}`));
  };

  const handleRestart = () => {
    const id = idRef.current;
    if (id) void api.terminalKill(id).catch(() => {});
    idRef.current = null;
    termRef.current?.writeln("\r\n--- restarting shell ---\r\n");
    void api
      .terminalSpawn(cwd ?? undefined)
      .then((newId) => {
        idRef.current = newId;
        setLabel("Shell");
      })
      .catch((err) =>
        termRef.current?.writeln(`\r\n[terminal] failed to start shell: ${err}`),
      );
  };

  return (
    <div
      className="flex shrink-0 flex-col border-t border-border bg-editor"
      style={height != null ? { height } : undefined}
    >
      <div className="flex items-center justify-between border-b border-border px-3 py-1">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
          Terminal
        </span>
        <div className="flex items-center gap-2">
          <span className="text-[9px] text-zinc-400">{label}</span>
          <button
            onClick={handleRestart}
            className="rounded px-1.5 py-0.5 text-[10px] text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
            title="Restart shell"
          >
            Restart
          </button>
        </div>
      </div>
      <div ref={hostRef} className="min-h-0 flex-1 overflow-hidden px-1.5 pt-1" />
      {ready && (
        <form
          className="flex items-center gap-1.5 border-t border-border px-2 py-1.5"
          onSubmit={(e) => {
            e.preventDefault();
            const input = e.currentTarget.elements.namedItem(
              "cmd",
            ) as HTMLInputElement;
            handleSubmit(input.value);
            input.value = "";
          }}
        >
          <span className="font-mono text-[11px] text-emerald-600">{label}$</span>
          <input
            name="cmd"
            autoComplete="off"
            spellCheck={false}
            placeholder="type a command…"
            className="min-w-0 flex-1 bg-transparent font-mono text-[12px] text-ink outline-none placeholder:text-zinc-500"
          />
        </form>
      )}
    </div>
  );
}
