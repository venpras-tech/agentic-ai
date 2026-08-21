import { useEffect, useRef, useState } from "react";

export interface ConsoleEntry {
  tool: string;
  stream: "stdout" | "stderr";
  chunk: string;
  ts: number;
}

interface ConsolePanelProps {
  entries: ConsoleEntry[];
  visible: boolean;
  onClear: () => void;
}

export default function ConsolePanel({ entries, visible, onClear }: ConsolePanelProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [filter, setFilter] = useState("");

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [entries.length]);

  if (!visible) return null;

  const filtered = filter
    ? entries.filter(
        (e) =>
          e.tool.toLowerCase().includes(filter.toLowerCase()) ||
          e.chunk.toLowerCase().includes(filter.toLowerCase()),
      )
    : entries;

  return (
    <div className="flex h-48 shrink-0 flex-col border-t border-border bg-editor">
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
        {filtered.length === 0 ? (
          <p className="py-2 text-zinc-400">No output yet — tool calls will appear here.</p>
        ) : (
          filtered.map((e, i) => (
            <div key={i} className="flex gap-2 py-px">
              <span className="shrink-0 text-zinc-400">{e.tool}</span>
              <span
                className={`flex-1 whitespace-pre-wrap ${
                  e.stream === "stderr" ? "text-red-500" : "text-zinc-600"
                }`}
              >
                {e.chunk}
              </span>
            </div>
          ))
        )}
      </div>
    </div>
  );
}
