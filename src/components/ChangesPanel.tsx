import { useMemo, useState } from "react";

import type { ChatMessage } from "../types";
import DiffView from "./DiffView";
import { api } from "../lib/ipc";

/**
 * One diff entry flattened from the transcript, carrying its position in the
 * message array so resolutions can be written back to the shared chat store.
 */
interface ChangeEntry {
  messageIndex: number;
  diffIndex: number;
  path: string;
  diff: string | undefined;
  before: string | undefined;
  resolved: "accepted" | "rejected" | undefined;
}

interface ChangesPanelProps {
  messages: ChatMessage[];
  onDiffResolve: (
    messageIndex: number,
    diffIndex: number,
    status: "accepted" | "rejected",
  ) => void;
}

/** Parse a unified diff into add/del counts without rendering. */
function counts(diff: string): { adds: number; dels: number } {
  let adds = 0;
  let dels = 0;
  for (const line of diff.split(/\r?\n/)) {
    if (line.startsWith("+")) adds++;
    else if (line.startsWith("-")) dels++;
  }
  return { adds, dels };
}

function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/**
 * "Changes" panel (Phase 8): aggregates every pending file diff attached to
 * the current chat transcript into one working-set browser with per-file and
 * bulk Accept / Revert — mirroring the source-control views of Cursor/Copilot.
 * Resolution state is shared with the chat timeline via the transcript store,
 * so accepting here updates the inline diff cards and vice-versa.
 */
export default function ChangesPanel({ messages, onDiffResolve }: ChangesPanelProps) {
  const [reverting, setReverting] = useState(false);

  const entries = useMemo<ChangeEntry[]>(() => {
    const all: ChangeEntry[] = [];
    messages.forEach((m, messageIndex) => {
      m.diffs?.forEach((d, diffIndex) => {
        all.push({
          messageIndex,
          diffIndex,
          path: d.path,
          diff: d.diff,
          before: d.before,
          resolved: d.resolved,
        });
      });
    });
    return all;
  }, [messages]);

  // Latest occurrence per path wins (later edits overwrite earlier hunks for
  // the same file), newest first overall.
  const byPath = useMemo(() => {
    const latest = new Map<string, ChangeEntry>();
    for (const e of entries) latest.set(e.path, e);
    return Array.from(latest.values()).reverse();
  }, [entries]);

  const pending = byPath.filter((e) => !e.resolved);
  const accepted = byPath.filter((e) => e.resolved === "accepted").length;
  const rejected = byPath.filter((e) => e.resolved === "rejected").length;

  const acceptAll = () => {
    for (const e of pending) onDiffResolve(e.messageIndex, e.diffIndex, "accepted");
  };

  const revertAll = async () => {
    if (reverting) return;
    setReverting(true);
    try {
      for (const e of pending) {
        if (!e.before) continue;
        try {
          await api.revertFile(e.path, e.before);
          onDiffResolve(e.messageIndex, e.diffIndex, "rejected");
        } catch {
          // leave as pending on failure — the per-file banner surfaces it
        }
      }
    } finally {
      setReverting(false);
    }
  };

  const revertOne = async (e: ChangeEntry) => {
    if (!e.before) return;
    try {
      await api.revertFile(e.path, e.before);
      onDiffResolve(e.messageIndex, e.diffIndex, "rejected");
    } catch {
      // silent — DiffView shows the inline revert error banner
    }
  };

  if (entries.length === 0) {
    return (
      <div className="min-h-0 flex-1 overflow-auto px-3 py-2">
        <p className="pt-3 text-center text-[11px] leading-relaxed text-zinc-400">
          No file changes in this conversation yet.
          <br />
          Diffs the agent proposes (write/diff events) appear here.
        </p>
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="shrink-0 space-y-1 border-b border-border px-2 py-1.5">
        <div className="flex items-center justify-between text-[10px] text-zinc-400">
          <span>
            {pending.length} pending · {accepted} accepted · {rejected} reverted
          </span>
          <span className="shrink-0 text-[9px] tabular-nums">
            {byPath.length} file{byPath.length === 1 ? "" : "s"}
          </span>
        </div>
        {pending.length > 0 && (
          <div className="flex items-center gap-1.5">
            <button
              onClick={acceptAll}
              className="flex-1 rounded bg-emerald-500/15 px-2 py-1 text-[10px] font-medium text-emerald-600 hover:bg-emerald-500/25"
            >
              ✓ Accept all ({pending.length})
            </button>
            <button
              onClick={() => void revertAll()}
              disabled={reverting}
              className="flex-1 rounded bg-red-500/15 px-2 py-1 text-[10px] font-medium text-red-600 hover:bg-red-500/25 disabled:opacity-50"
            >
              {reverting ? "Reverting…" : `↩ Revert all (${pending.length})`}
            </button>
          </div>
        )}
      </div>
      <div className="min-h-0 flex-1 space-y-1.5 overflow-auto px-1.5 py-1.5">
        {byPath.map((e) => {
          const c = counts(e.diff ?? "");
          return (
            <div key={e.path} className="overflow-hidden rounded-md border border-border bg-panel-2/40">
              <div className="flex items-center gap-1.5 px-2 py-1">
                <span className="min-w-0 flex-1 truncate font-mono text-[10px] text-zinc-600">
                  {baseName(e.path)}
                </span>
                <span className="shrink-0 text-[9px] tabular-nums text-zinc-400">
                  <span className="text-emerald-500">+{c.adds}</span>{" "}
                  <span className="text-red-500">-{c.dels}</span>
                </span>
                {!e.resolved ? (
                  <>
                    <button
                      onClick={() => onDiffResolve(e.messageIndex, e.diffIndex, "accepted")}
                      aria-label={`Accept changes to ${baseName(e.path)}`}
                      className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-[9px] font-medium text-emerald-600 hover:bg-emerald-500/20"
                    >
                      Accept
                    </button>
                    {e.before && (
                      <button
                        onClick={() => void revertOne(e)}
                        aria-label={`Revert changes to ${baseName(e.path)}`}
                        className="rounded bg-red-500/10 px-1.5 py-0.5 text-[9px] font-medium text-red-600 hover:bg-red-500/20"
                      >
                        Revert
                      </button>
                    )}
                  </>
                ) : (
                  <span
                    className={`shrink-0 text-[9px] font-medium ${
                      e.resolved === "accepted" ? "text-emerald-600" : "text-red-600"
                    }`}
                  >
                    {e.resolved === "accepted" ? "Accepted" : "Reverted"}
                  </span>
                )}
              </div>
              <div className="px-1.5 pb-1.5">
                <DiffView
                  path={e.path}
                  diff={e.diff ?? ""}
                  before={e.before}
                  resolved={e.resolved ?? "pending"}
                  onResolved={(ok) =>
                    onDiffResolve(
                      e.messageIndex,
                      e.diffIndex,
                      ok ? "accepted" : "rejected",
                    )
                  }
                />
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}