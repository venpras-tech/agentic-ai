import "./taskChecklist.css";

export interface TaskChecklistItem {
  id: number;
  title: string;
  done: boolean;
}

/**
 * Sticky focus-chain checklist (P5.5): shows the agent's current todo list at
 * the top of the transcript while a run is in progress, so progress stays
 * visible without opening the timeline. Renders nothing once every item is
 * done, letting it collapse away on its own.
 */
function TaskChecklist({ items }: { items: TaskChecklistItem[] }) {
  if (items.length === 0) return null;
  const done = items.filter((t) => t.done).length;
  const allDone = done === items.length;
  const current = items.find((t) => !t.done);

  return (
    <div className={`task-checklist${allDone ? " task-checklist--done" : ""}`} role="status">
      <div className="task-checklist-head">
        <span className="task-checklist-title">
          {allDone ? "All tasks complete" : current ? current.title : "Working"}
        </span>
        <span className="task-checklist-count">
          {done}/{items.length}
        </span>
      </div>
      <div
        className="task-checklist-bar"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={items.length}
        aria-valuenow={done}
      >
        <span
          className="task-checklist-bar-fill"
          style={{ width: `${items.length ? (done / items.length) * 100 : 0}%` }}
        />
      </div>
      <ul className="task-checklist-list">
        {items.slice(0, 6).map((t) => (
          <li key={t.id} className={`task-checklist-item${t.done ? " done" : ""}`}>
            <span className="task-checklist-mark">{t.done ? "✓" : "○"}</span>
            <span className="task-checklist-label" title={t.title}>
              {t.title}
            </span>
          </li>
        ))}
        {items.length > 6 && (
          <li className="task-checklist-more">+{items.length - 6} more…</li>
        )}
      </ul>
    </div>
  );
}

export default TaskChecklist;
