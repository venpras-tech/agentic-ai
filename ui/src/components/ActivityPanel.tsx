import "./ActivityPanel.css";

interface ActivityLogEntry {
  text: string;
  type: "status" | "error" | "tool" | "index";
}

interface ActivityPanelProps {
  logs: ActivityLogEntry[];
  collapsed: boolean;
  onToggle: () => void;
  /** Case-insensitive substring filter over `text` (set by "View log"). */
  filter?: string;
  onClearFilter?: () => void;
}

function ActivityPanel({ logs, collapsed, onToggle, filter, onClearFilter }: ActivityPanelProps) {
  const latestStatus = logs.filter((l) => l.type === "status").slice(-1)[0];
  const needle = filter?.trim().toLowerCase() ?? "";
  const visible = needle
    ? logs.filter((l) => l.text.toLowerCase().includes(needle))
    : logs;

  return (
    <div className={`activity-wrap ${collapsed ? "collapsed" : ""}`}>
      <div className="activity-header">
        <span className="activity-title">Backend activity</span>
        {latestStatus && (
          <span className="activity-status-live">{latestStatus.text}</span>
        )}
        {needle && (
          <span className="activity-filter-chip" title="Filtered activity">
            filter: {filter}
            {onClearFilter && (
              <button
                type="button"
                className="activity-filter-clear"
                onClick={onClearFilter}
                title="Clear filter"
              >
                ×
              </button>
            )}
          </span>
        )}
        <button
          type="button"
          className="activity-toggle"
          onClick={onToggle}
          title="Toggle activity panel"
          aria-expanded={!collapsed}
        >
          ▼
        </button>
      </div>
      <div className="activity">
        {visible.length === 0 && needle && (
          <div className="activity-item activity-status">
            <span className="activity-label">Status</span>
            <span className="activity-body">No activity matches “{filter}”.</span>
          </div>
        )}
        {visible.map((log, index) => (
          <div key={index} className={`activity-item activity-${log.type}`}>
            <span className="activity-label">
              {log.type === "tool" ? "Tool" :
               log.type === "error" ? "Error" :
               log.type === "index" ? "Index" : "Status"}
            </span>
            <span className="activity-body">{log.text}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export default ActivityPanel;