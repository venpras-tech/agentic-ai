import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { api } from "../lib/ipc";

interface DiffLine {
  tag: "header" | "add" | "del" | "ctx" | "meta";
  text: string;
}

function parseUnifiedDiff(raw: string): DiffLine[] {
  const lines = raw.split(/\r?\n/);
  const out: DiffLine[] = [];
  for (const line of lines) {
    if (line.startsWith("+++") || line.startsWith("---")) {
      out.push({ tag: "meta", text: line });
    } else if (line.startsWith("@@")) {
      out.push({ tag: "header", text: line });
    } else if (line.startsWith("+")) {
      out.push({ tag: "add", text: line });
    } else if (line.startsWith("-")) {
      out.push({ tag: "del", text: line });
    } else {
      out.push({ tag: "ctx", text: line });
    }
  }
  return out;
}

const TAG_CLASS: Record<DiffLine["tag"], string> = {
  header: "bg-cyan-500/10 text-cyan-600/90",
  add: "bg-emerald-500/10 text-emerald-600/90",
  del: "bg-red-500/10 text-red-600/90",
  ctx: "text-zinc-500",
  meta: "text-zinc-400",
};

const ROW_HEIGHT = 16;
const OVERSCAN = 30;

interface DiffViewProps {
  path: string;
  diff: string;
  /** Pre-change content for revert. */
  before?: string;
  /** Called when the diff is accepted or rejected. */
  onResolved?: (accepted: boolean) => void;
}

export default function DiffView({ path, diff, before, onResolved }: DiffViewProps) {
  const [status, setStatus] = useState<"pending" | "accepted" | "rejected">("pending");
  const [revertError, setRevertError] = useState<string | null>(null);
  const { lines, adds, dels } = useMemo(() => {
    const parsed = parseUnifiedDiff(diff);
    let a = 0;
    let d = 0;
    for (const l of parsed) {
      if (l.tag === "add") a++;
      else if (l.tag === "del") d++;
    }
    return { lines: parsed, adds: a, dels: d };
  }, [diff]);
  const short = path.split(/[\\/]/).pop() ?? path;

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [start, setStart] = useState(0);
  const [end, setEnd] = useState(50);

  const updateWindow = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const first = Math.max(0, Math.floor(el.scrollTop / ROW_HEIGHT) - OVERSCAN);
    const visible = Math.ceil(el.clientHeight / ROW_HEIGHT) + OVERSCAN * 2;
    setStart(first);
    setEnd(first + visible);
  }, []);

  useLayoutEffect(() => {
    updateWindow();
  }, [lines.length, updateWindow]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onScroll = () => updateWindow();
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [updateWindow]);

  const handleAccept = () => {
    setStatus("accepted");
    onResolved?.(true);
  };

  const handleReject = async () => {
    if (!before) return;
    setRevertError(null);
    try {
      await api.revertFile(path, before);
      setStatus("rejected");
      onResolved?.(false);
    } catch (e) {
      setRevertError(String(e));
    }
  };

  return (
    <div className="rounded-md border border-border bg-zinc-100">
      <div className="flex items-center justify-between gap-2 border-b border-border px-2 py-1">
        <span className="truncate font-mono text-[10px] text-zinc-500">{short}</span>
        <span className="flex items-center gap-1.5">
          <span className="shrink-0 text-[9px] tabular-nums text-zinc-400">
            <span className="text-emerald-500">+{adds}</span>{" "}
            <span className="text-red-500">-{dels}</span>
          </span>
          {status === "pending" && (
            <>
              <button
                type="button"
                onClick={handleAccept}
                className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-[9px] font-medium text-emerald-600 hover:bg-emerald-500/20"
              >
                Accept
              </button>
              {before && (
                <button
                  type="button"
                  onClick={handleReject}
                  className="rounded bg-red-500/10 px-1.5 py-0.5 text-[9px] font-medium text-red-600 hover:bg-red-500/20"
                >
                  Reject
                </button>
              )}
            </>
          )}
          {status === "accepted" && (
            <span className="text-[9px] font-medium text-emerald-600">Accepted</span>
          )}
          {status === "rejected" && (
            <span className="text-[9px] font-medium text-red-600">Reverted</span>
          )}
        </span>
      </div>
      {revertError && (
        <div className="border-b border-red-500/30 bg-red-500/10 px-2 py-1 text-[9px] text-red-600">
          Revert failed: {revertError}
        </div>
      )}
      <div
        ref={scrollRef}
        onScroll={updateWindow}
        className="max-h-64 overflow-auto py-1 font-mono text-[10px] leading-snug"
      >
        <div style={{ height: lines.length * ROW_HEIGHT, position: "relative" }}>
          {lines.slice(start, end).map((l, i) => (
            <div
              key={start + i}
              className={`whitespace-pre-wrap px-2 ${TAG_CLASS[l.tag]}`}
              style={{ position: "absolute", top: (start + i) * ROW_HEIGHT, left: 0, right: 0 }}
            >
              {l.text || " "}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
