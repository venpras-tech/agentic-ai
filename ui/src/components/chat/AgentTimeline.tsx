import { useState } from "react";
import type {
  BackgroundTaskState,
  PlanStepState,
  SubagentRunState,
  SubtaskState,
  TodoItemState,
} from "../../hooks/useAgentStream";
import BgTaskOutputDrawer from "./BgTaskOutputDrawer";
import "./agentTimeline.css";

interface AgentTimelineProps {
  planSteps: PlanStepState[];
  subtasks: SubtaskState[];
  subagentRuns?: SubagentRunState[];
  todos: TodoItemState[];
  bgTasks: BackgroundTaskState[];
  onAbortBackground?: (taskId: string) => void;
  onViewLog?: (query: string) => void;
}

const STEP_ICON: Record<string, string> = {
  completed: "✓",
  terminal: "✓",
  in_progress: "▸",
  failed: "✕",
};

const TODO_ICON: Record<string, string> = {
  running: "▸",
  done: "✓",
  completed: "✓",
  failed: "✕",
  error: "✕",
  aborted: "⊘",
};

const RUN_ICON: Record<string, string> = {
  started: "▸",
  running: "▸",
  completed: "✓",
  done: "✓",
  error: "✕",
  failed: "✕",
};

/**
 * Live view of the agent's own bookkeeping: plan progress, decomposition
 * subtasks, first-class subagent runs, the todo list, and background task
 * pills. Renders nothing when the backend has not reported any of it, so idle
 * chats stay uncluttered.
 */
function AgentTimeline({
  planSteps,
  subtasks,
  subagentRuns = [],
  todos,
  bgTasks,
  onAbortBackground,
  onViewLog,
}: AgentTimelineProps) {
  const hasPlan = planSteps.length > 0;
  const hasSubtasks = subtasks.length > 0;
  const hasRuns = subagentRuns.length > 0;
  const hasTodos = todos.length > 0;
  const hasBg = bgTasks.length > 0;
  const [openBgTaskId, setOpenBgTaskId] = useState<string | null>(null);
  if (!hasPlan && !hasSubtasks && !hasRuns && !hasTodos && !hasBg) return null;

  const doneTodos = todos.filter((t) => t.done).length;
  const runs = [...subagentRuns].sort((a, b) => a.startedAt - b.startedAt);

  return (
    <div className="agent-timeline">
      {hasPlan && (
        <section className="tl-section" aria-label="Plan progress">
          <h4 className="tl-title">Plan</h4>
          <ol className="tl-steps">
            {planSteps.map((s) => (
              <li key={s.index} className={`tl-step tl-${s.status}`}>
                <span className="tl-icon" aria-hidden="true">
                  {STEP_ICON[s.status] ?? "•"}
                </span>
                <span className="tl-text">{s.title}</span>
                {s.error && <span className="tl-error">{s.error}</span>}
              </li>
            ))}
          </ol>
        </section>
      )}

      {hasSubtasks && (
        <section className="tl-section" aria-label="Subtasks">
          <h4 className="tl-title">Subtasks</h4>
          <ol className="tl-steps">
            {subtasks.map((s) => (
              <li key={s.index} className={`tl-step tl-${s.status}`}>
                <span className="tl-icon" aria-hidden="true">
                  {TODO_ICON[s.status] ?? "•"}
                </span>
                <span className="tl-text">
                  {s.total ? `${s.index}/${s.total} ` : ""}
                  {s.title}
                </span>
              </li>
            ))}
          </ol>
        </section>
      )}

      {hasRuns && (
        <section className="tl-section" aria-label="Subagent runs">
          <h4 className="tl-title">Subagents</h4>
          <ul className="tl-steps">
            {runs.map((r) => (
              <li
                key={r.key}
                className={`tl-step tl-${r.status === "completed" ? "completed" : r.status}`}
              >
                <span className="tl-icon" aria-hidden="true">
                  {RUN_ICON[r.status] ?? "•"}
                </span>
                <span className="tl-text">
                  <span className="tl-run-profile">{r.profile}</span>
                  {r.summary ? ` — ${r.summary}` : ""}
                  {typeof r.durationMs === "number" && (
                    <span className="tl-run-meta"> · {(r.durationMs / 1000).toFixed(1)}s</span>
                  )}
                  {typeof r.leasedWorkers === "number" && r.leasedWorkers > 0 && (
                    <span className="tl-run-meta"> · {r.leasedWorkers} worker(s)</span>
                  )}
                  {onViewLog && (
                    <button
                      type="button"
                      className="tl-run-log"
                      onClick={() => onViewLog(r.profile)}
                      title={`Filter activity to “${r.profile}” subagent runs`}
                    >
                      View log
                    </button>
                  )}
                </span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {hasTodos && (
        <section className="tl-section" aria-label="Todo list">
          <h4 className="tl-title">
            Todos <span className="tl-count">{doneTodos}/{todos.length}</span>
          </h4>
          <ul className="tl-steps">
            {todos.map((t) => (
              <li key={t.id} className={`tl-step ${t.done ? "tl-completed" : "tl-in_progress"}`}>
                <span className="tl-icon" aria-hidden="true">
                  {t.done ? "☑" : "☐"}
                </span>
                <span className="tl-text">{t.title}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      {hasBg && (
        <section className="tl-section" aria-label="Background tasks">
          <h4 className="tl-title">Background</h4>
          <div className="tl-bg-pills">
            {bgTasks.map((t) => (
              <span
                key={t.id}
                className={`tl-bg-pill tl-bg-${t.status}`}
                title={t.detail || undefined}
              >
                <button
                  type="button"
                  className="tl-bg-label"
                  onClick={() => setOpenBgTaskId(openBgTaskId === t.id ? null : t.id)}
                  title="Show task output"
                >
                  {t.label}
                </button>
                {onAbortBackground && t.status === "running" && (
                  <button
                    type="button"
                    className="tl-bg-abort"
                    onClick={() => onAbortBackground(t.id)}
                    title="Abort background task"
                  >
                    ×
                  </button>
                )}
              </span>
            ))}
          </div>
          {openBgTaskId &&
            bgTasks
              .filter((t) => t.id === openBgTaskId)
              .map((t) => (
                <BgTaskOutputDrawer
                  key={t.id}
                  taskId={t.id}
                  label={t.label}
                  running={t.status === "running"}
                  onClose={() => setOpenBgTaskId(null)}
                  onAbort={onAbortBackground ? () => onAbortBackground(t.id) : undefined}
                />
              ))}
        </section>
      )}
    </div>
  );
}

export default AgentTimeline;
