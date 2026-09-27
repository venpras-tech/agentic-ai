import type {
  BackgroundTaskState,
  PlanStepState,
  SubtaskState,
  TodoItemState,
} from "../../hooks/useAgentStream";
import "./agentTimeline.css";

interface AgentTimelineProps {
  planSteps: PlanStepState[];
  subtasks: SubtaskState[];
  todos: TodoItemState[];
  bgTasks: BackgroundTaskState[];
  onAbortBackground?: (taskId: string) => void;
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

/**
 * Live view of the agent's own bookkeeping: plan progress, decomposition
 * subtasks, the todo list, and background task pills. Renders nothing when the
 * backend has not reported any of it, so idle chats stay uncluttered.
 */
function AgentTimeline({
  planSteps,
  subtasks,
  todos,
  bgTasks,
  onAbortBackground,
}: AgentTimelineProps) {
  const hasPlan = planSteps.length > 0;
  const hasSubtasks = subtasks.length > 0;
  const hasTodos = todos.length > 0;
  const hasBg = bgTasks.length > 0;
  if (!hasPlan && !hasSubtasks && !hasTodos && !hasBg) return null;

  const doneTodos = todos.filter((t) => t.done).length;

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
                <span className="tl-bg-label">{t.label}</span>
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
        </section>
      )}
    </div>
  );
}

export default AgentTimeline;
