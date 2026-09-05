import { useEffect, useRef, useState } from "react";

export interface ConsoleEntry {
  tool: string;
  chunk: string;
  ts: number;
}

interface ConsolePanelProps {
  entries: ConsoleEntry[];
  visible: boolean;
  /** Draggable height in px; falls back to the old fixed h-48. */
  height?: number;
  onClear: () => void;
}

type Severity = "error" | "warn" | "info";

/** Classify by content so INFO lines are neutral and only real problems get color. */
function severityOf(chunk: string): Severity {
  if (/\bERROR\b|\bError:|panicked|FAILED/i.test(chunk)) return "error";
  if (/\bWARN(?:ING)?\b/i.test(chunk)) return "warn";
  return "info";
}

const LEVEL_COLOR: Record<Severity, string> = {
  error: "text-red-500",
  warn: "text-amber-500",
  info: "text-zinc-600",
};

const LEVEL_BADGE: Record<Severity, string> = {
  error: "bg-red-500/15 text-red-600",
  warn: "bg-amber-500/15 text-amber-600",
  info: "bg-zinc-500/15 text-zinc-500",
};

/** `YYYY-MM-DD HH:MM:SS.mmm` (local), matching the backend log format. */
const pad = (n: number, w = 2) => String(n).padStart(w, "0");
const clock = (ts: number) => {
  const d = new Date(ts);
  return (
    `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ` +
    `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}.` +
    `${pad(d.getMilliseconds(), 3)}`
  );
};

export default function ConsolePanel({ entries, visible, height, onClear }: ConsolePanelProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [filter, setFilter] = useState("");

  // Newest lines sit on top, so "follow the tail" means pinning to the top.
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = 0;
  }, [entries.length]);

  if (!visible) return null;

  const needle = filter.toLowerCase();
  const filtered = needle
    ? entries.filter(
        (e) =>
          e.tool.toLowerCase().includes(needle) ||
          e.chunk.toLowerCase().includes(needle),
      )
    : entries;

  // Render newest-first without mutating the append-only ring buffer.
  const ordered = [...filtered].reverse();

  return (
    <div
      className="flex shrink-0 select-text flex-col border-t border-border bg-editor"
      style={height != null ? { height } : undefined}
    >
      <div className="flex items-center justify-between border-b border-border px-3 py-1">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
          Console
        </span>
        <div className="flex items-center gap-2">
          <input
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter…"
            className="w-32 rounded border border-border bg-panel-2 px-1.5 py-0.5 text-[10px] text-ink outline-none placeholder:text-zinc-400 focus:border-accent/60"
          />
          <span className="text-[9px] text-zinc-400">{filtered.length} entries</span>
          <button
            onClick={onClear}
            className="rounded px-1.5 py-0.5 text-[10px] text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
          >
            Clear
          </button>
        </div>
      </div>
      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-3 py-1 font-mono text-[10px] leading-snug">
        {ordered.length === 0 ? (
          <p className="py-2 text-zinc-400">No output yet — logs will appear here.</p>
        ) : (
          ordered.map((e, i) => {
            const sev = severityOf(e.chunk);
            return (
              <div key={`${e.ts}-${i}`} className="flex items-start gap-2 py-px">
                <span className="shrink-0 text-zinc-400">{clock(e.ts)}</span>
                <span
                  className={`shrink-0 rounded px-1 py-px text-[9px] font-semibold ${LEVEL_BADGE[sev]}`}
                >
                  {e.tool}
                </span>
                <span className={`flex-1 whitespace-pre-wrap break-all ${LEVEL_COLOR[sev]}`}>
                  {e.chunk}
                </span>
              </div>
            );
          })
        )}
      </div>
    </div>
  );
}
