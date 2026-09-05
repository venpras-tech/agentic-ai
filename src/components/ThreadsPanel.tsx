import { useEffect, useState } from "react";
import type { BackgroundTaskInfo, LedgerEntry } from "../types";
import {
  isActivePhase,
  statusView,
  type ChatStatus,
} from "../lib/chatStatus";
import type { SubtaskRun } from "../stores/agentRunStore";

interface ThreadsPanelProps {
  chatStatus: ChatStatus;
  activeSessionId: number | null;
  currentStep: number | null;
  currentSubtask: SubtaskRun | null;
  /** Every sub-task still running (first-class `task` batches run in parallel). */
  runningSubtasks: SubtaskRun[];
  modelName: string | null;
  ledger: LedgerEntry[];
  backgroundTasks: BackgroundTaskInfo[];
  onAbort: (taskId: string) => void;
}

function formatMs(ms: number): string {
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)}m`;
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.round(ms)}ms`;
}

function formatTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(1)}k`;
  return String(n);
}

export default function ThreadsPanel({
  chatStatus,
  activeSessionId,
  currentStep,
  runningSubtasks,
  modelName,
  ledger,
  backgroundTasks,
  onAbort,
}: ThreadsPanelProps) {
  // Ticking clock so live rows (active run + background tasks) update elapsed.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  const active = isActivePhase(chatStatus.phase);
  const view = statusView(chatStatus, now);
  const { label, phase } = view;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex items-center gap-2 border-b border-border px-3 py-2">
        <span className="h-1.5 w-1.5 rounded-full bg-cyan-500" />
        <span className="text-[11px] font-semibold uppercase tracking-wider text-zinc-600">
          Threads
        </span>
        <span className="ml-auto text-[9px] text-zinc-400">
          {modelName ?? "no model"}
        </span>
      </div>
      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto p-2">
        {/* Foreground agent run */}
        <div
          className={`rounded-md border p-2 ${
            active ? "border-cyan-500/40 bg-cyan-500/5" : "border-border bg-panel"
          }`}
        >
          <div className="flex items-center gap-1.5">
            <span
              className={`h-1.5 w-1.5 shrink-0 rounded-full ${
                active ? "animate-pulse bg-cyan-500" : "bg-zinc-300"
              }`}
            />
            <span className="truncate text-[11px] font-medium text-zinc-700">
              Foreground
            </span>
            <span className="ml-auto shrink-0 rounded bg-panel-2 px-1 py-px text-[9px] font-medium uppercase tracking-wide text-zinc-500">
              {phase}
            </span>
          </div>
          {active ? (
            <>
              {label && <div className="mt-1 truncate text-[10.5px] text-cyan-700">{label}</div>}
              <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[9.5px] text-zinc-500">
                {activeSessionId != null && (
                  <span>session {activeSessionId}</span>
                )}
                {currentStep != null && <span>step {currentStep}</span>}
                {chatStatus.sinceMs > 0 && (
                  <span>{formatMs(now - chatStatus.sinceMs)}</span>
                )}
              </div>
              {/* Row-by-row sub-agent status: model + elapsed + active tool. */}
              {runningSubtasks.length > 0 && (
                <div className="mt-1.5 space-y-1">
                  {[...runningSubtasks]
                    .sort((a, b) => a.index - b.index)
                    .map((s) => (
                      <div
                        key={`${s.index}/${s.total}/${s.title}`}
                        className="rounded border border-cyan-500/20 bg-cyan-500/10 px-1.5 py-1"
                      >
                        <div className="truncate text-[10px] font-medium text-cyan-800">
                          subtask {s.index}/{s.total} · {s.title}
                        </div>
                        <div className="mt-0.5 flex flex-wrap items-center gap-x-2 text-[9px] text-zinc-500">
                          {s.model && (
                            <span className="max-w-[10rem] truncate" title={s.model}>
                              {s.model}
                            </span>
                          )}
                          {s.tool && (
                            <span
                              className="max-w-[10rem] truncate text-cyan-700"
                              title={s.tool}
                            >
                              ⎇ {s.tool}
                            </span>
                          )}
                          <span className="tabular-nums">
                            {s.startedAt != null
                              ? formatMs(now - s.startedAt)
                              : "…"}
                          </span>
                        </div>
                      </div>
                    ))}
                </div>
              )}
            </>
          ) : (
            <div className="mt-1 text-[10.5px] text-zinc-400">
              {backgroundTasks.length > 0
                ? "Idle — background tasks running"
                : "Idle"}
            </div>
          )}
        </div>

        {/* Background tasks */}
        <div>
          <div className="px-0.5 pt-1 text-[9px] font-semibold uppercase tracking-wider text-zinc-400">
            Background
          </div>
          {backgroundTasks.length === 0 ? (
            <div className="px-0.5 py-1 text-[10px] text-zinc-400">No background tasks</div>
          ) : (
            <div className="space-y-1">
              {backgroundTasks.map((t) => (
                <div key={t.id} className="flex items-center gap-1.5 rounded-md border border-border bg-panel px-2 py-1.5">
                  <span className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-amber-500" />
                  <div className="min-w-0 flex-1">
                    <div className="truncate text-[11px] text-zinc-700">
                      {t.label || t.id}
                    </div>
                    <div className="text-[9.5px] text-zinc-400">
                      {t.sessionId != null && `session ${t.sessionId} · `}
                      {formatMs(now - t.startedAt)}
                    </div>
                  </div>
                  <button
                    onClick={() => onAbort(t.id)}
                    className="shrink-0 rounded px-1.5 py-0.5 text-[10px] text-red-500 hover:bg-red-50 hover:text-red-700"
                    title="Abort this task"
                  >
                    stop
                  </button>
                </div>
              ))}
            </div>
          )}
        </div>

        {/* Recent session ledger */}
        <div>
          <div className="px-0.5 pt-1 text-[9px] font-semibold uppercase tracking-wider text-zinc-400">
            Recent sessions
          </div>
          {ledger.length === 0 ? (
            <div className="px-0.5 py-1 text-[10px] text-zinc-400">
              No completed sessions yet
            </div>
          ) : (
            <div className="space-y-1">
              {[...ledger].reverse().map((l) => (
                <div key={l.sessionId} className="flex items-center gap-1.5 rounded-md border border-border bg-panel px-2 py-1.5">
                  <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-emerald-400" />
                  <span className="truncate text-[11px] text-zinc-700">
                    {l.label || `session ${l.sessionId}`}
                  </span>
                  <span className="ml-auto shrink-0 text-[9.5px] text-zinc-400">
                    {formatTokens(l.tokens)} tok · {l.toolCalls} tool(s) ·{" "}
                    {formatMs(l.elapsedMs)}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
