import { useState } from "react";
import type { BackgroundTaskInfo } from "../types";

interface BackgroundTasksProps {
  tasks: BackgroundTaskInfo[];
  onAbort: (taskId: string) => void;
}

function elapsed(ms: number): string {
  if (ms >= 60_000) return `${(ms / 60_000).toFixed(1)}m`;
  if (ms >= 1000) return `${(ms / 1000).toFixed(0)}s`;
  return `${Math.round(ms)}ms`;
}

export default function BackgroundTasks({ tasks, onAbort }: BackgroundTasksProps) {
  const [expanded, setExpanded] = useState(false);

  if (tasks.length === 0) return null;

  return (
    <div className="pointer-events-auto">
      <button
        onClick={() => setExpanded((v) => !v)}
        className="flex items-center gap-1.5 rounded-full border border-amber-400/40 bg-amber-50 px-2.5 py-0.5 text-[10.5px] font-medium text-amber-700 shadow-sm transition-colors hover:bg-amber-100"
        title={`${tasks.length} background task${tasks.length === 1 ? "" : "s"} running`}
      >
        <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-500" />
        {tasks.length} background{tasks.length === 1 ? "" : "s"}
      </button>
      {expanded && (
        <div className="absolute bottom-8 right-4 z-40 w-72 rounded-lg border border-border bg-panel shadow-lg">
          <div className="flex items-center justify-between border-b border-border px-3 py-1.5 text-[10.5px] font-semibold text-zinc-600">
            <span>Background Tasks</span>
            <button
              onClick={() => setExpanded(false)}
              className="text-zinc-400 hover:text-zinc-700"
            >
              ×
            </button>
          </div>
          <div className="max-h-48 overflow-y-auto">
            {tasks.map((t) => (
              <div
                key={t.id}
                className="flex items-center gap-2 border-b border-border/50 px-3 py-2 last:border-b-0"
              >
                <span className="h-1.5 w-1.5 shrink-0 animate-pulse rounded-full bg-amber-500" />
                <div className="min-w-0 flex-1">
                  <div className="truncate text-[11px] text-zinc-700">
                    {t.label || t.id}
                  </div>
                  <div className="text-[10px] text-zinc-400">
                    {elapsed(t.durationMs ?? (Date.now() - t.startedAt))}
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
        </div>
      )}
    </div>
  );
}

/** Compact inline pill for a single background task (used in the status bar). */
export function BackgroundTaskPill({
  task,
  onAbort,
}: {
  task: BackgroundTaskInfo;
  onAbort: (taskId: string) => void;
}) {
  return (
    <span className="inline-flex items-center gap-1 rounded-full border border-amber-400/30 bg-amber-50 px-2 py-0.5 text-[10px] text-amber-700">
      <span className="h-1 w-1 animate-pulse rounded-full bg-amber-500" />
      <span className="max-w-[120px] truncate">{task.label || task.id}</span>
      <button
        onClick={() => onAbort(task.id)}
        className="ml-0.5 text-amber-400 hover:text-red-600"
        title="Abort"
      >
        ×
      </button>
    </span>
  );
}
