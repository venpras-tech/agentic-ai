import "./ActivityPanel.css";

interface ActivityLogEntry {
  text: string;
  type: "status" | "error" | "tool" | "index";
}

interface ActivityPanelProps {
  logs: ActivityLogEntry[];
  collapsed: boolean;
  onToggle: () => void;
}

function ActivityPanel({ logs, collapsed, onToggle }: ActivityPanelProps) {
  const latestStatus = logs.filter((l) => l.type === "status").slice(-1)[0];

  return (
    <div className={`activity-wrap ${collapsed ? "collapsed" : ""}`}>
      <div className="activity-header">
        <span className="activity-title">Backend activity</span>
        {latestStatus && (
          <span className="activity-status-live">{latestStatus.text}</span>
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
        {logs.map((log, index) => (
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