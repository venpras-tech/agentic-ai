import { useEffect, useRef, useState } from "react";

import type { AuditEntry } from "../types";
import { api } from "../lib/ipc";

function timeAgo(ts: number): string {
  const d = Math.max(0, Date.now() - ts);
  if (d < 1000) return "now";
  if (d < 60_000) return `${Math.round(d / 1000)}s ago`;
  if (d < 3_600_000) return `${Math.round(d / 60_000)}m ago`;
  return `${Math.round(d / 3_600_000)}h ago`;
}

function decisionLabel(decision: string): { text: string; tone: string } {
  switch (decision) {
    case "allow":
      return { text: "allow", tone: "bg-emerald-500/15 text-emerald-600" };
    case "granted":
      return { text: "granted", tone: "bg-emerald-500/15 text-emerald-600" };
    case "granted-session":
      return { text: "granted (session)", tone: "bg-emerald-500/15 text-emerald-600" };
    case "granted-always":
      return { text: "granted (always)", tone: "bg-emerald-500/15 text-emerald-600" };
    case "deny":
      return { text: "denied", tone: "bg-red-500/15 text-red-600" };
    case "declined":
      return { text: "declined", tone: "bg-red-500/15 text-red-600" };
    case "timed-out":
      return { text: "timed out", tone: "bg-amber-500/15 text-amber-600" };
    case "aborted":
      return { text: "aborted", tone: "bg-zinc-500/15 text-zinc-500" };
    default:
      return { text: decision, tone: "bg-zinc-500/15 text-zinc-500" };
  }
}

export default function AuditMenu() {
  const [open, setOpen] = useState(false);
  const [entries, setEntries] = useState<AuditEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  const refresh = () => {
    setLoading(true);
    api
      .agentAuditLog(50)
      .then(setEntries)
      .catch(() => setEntries([]))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    if (!open) return;
    refresh();
    const onDoc = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen((v) => !v)}
        title="Tool audit trail — every policy decision (.ai/audit.jsonl)"
        className="flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium text-zinc-500 transition-colors hover:bg-panel hover:text-zinc-800"
      >
        ◷ {entries.length > 0 ? entries.length : ""}
      </button>
      {open && (
        <div className="absolute bottom-6 right-0 z-40 w-80 overflow-hidden rounded-lg border border-border bg-panel-2 shadow-2xl">
          <div className="border-b border-border px-3 py-2">
            <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
              Audit trail
            </span>
              <p className="mt-0.5 text-[9px] leading-snug text-zinc-500">
              Every tool-call policy decision, newest first — persisted to{" "}
              <span className="font-mono">.ai/audit.jsonl</span>.
            </p>
          </div>
          <div className="max-h-72 overflow-y-auto p-1.5">
            {loading ? (
              <p className="px-2 py-2 text-[10px] text-zinc-500">Loading…</p>
            ) : entries.length === 0 ? (
              <p className="px-2 py-2 text-[10px] text-zinc-500">
                No tool calls yet — decisions land here as the agent runs.
              </p>
            ) : (
              entries.map((en, i) => {
                const d = decisionLabel(en.decision);
                return (
                  <div
                    key={`${en.id}-${i}`}
                    className="flex items-start gap-2 rounded px-2 py-1.5 hover:bg-panel"
                    title={en.error ?? en.summary}
                  >
                    <span
                      className={`mt-0.5 shrink-0 rounded px-1 py-px text-[8.5px] font-semibold normal-case ${d.tone}`}
                    >
                      {d.text}
                    </span>
                    <span className="min-w-0 flex-1 truncate font-mono text-[9.5px] text-zinc-500">
                      {en.tool}
                    </span>
                    <span className="shrink-0 text-[9px] text-zinc-500">
                      {timeAgo(en.ts)}
                    </span>
                  </div>
                );
              })
            )}
          </div>
          {entries.length > 0 && (
            <div className="border-t border-border px-3 py-1.5 text-[9px] text-zinc-500">
              {entries.length} recent · {entries.filter((e) => e.success === true).length} ok ·{" "}
              {entries.filter((e) => e.success === false).length} blocked/failed
            </div>
          )}
        </div>
      )}
    </div>
  );
}
