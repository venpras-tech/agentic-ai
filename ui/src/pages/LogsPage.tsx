import { useState, useEffect } from "react";
import { api } from "../lib/ipc";
import "./LogsPage.css";

const logSources = [
  { id: "all", label: "All" },
  { id: "console", label: "Console" },
];

function LogsPage() {
  const [logType, setLogType] = useState("all");
  const [logs, setLogs] = useState<string[]>([]);
  const [searchQuery, setSearchQuery] = useState("");
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    void loadLogs();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const loadLogs = async () => {
    setLoading(true);
    try {
      const history = await api.consoleHistory();
      setLogs([...(history || [])].reverse());
    } catch {
      setLogs(["Backend logs are only available inside the Tauri shell."]);
    }
    setLoading(false);
  };

  const handleCopy = () => {
    const text = filteredLogs.join("\n");
    void navigator.clipboard.writeText(text);
  };

  const filteredLogs = searchQuery
    ? logs.filter((log) => log.toLowerCase().includes(searchQuery.toLowerCase()))
    : logs;

  const getLogClass = (line: string) => {
    if (line.includes("ERROR") || line.includes("error")) return "log-error";
    if (line.includes("WARNING") || line.includes("warn")) return "log-warning";
    if (line.includes("INFO")) return "log-info";
    if (line.includes("DEBUG")) return "log-debug";
    if (line.includes("Traceback")) return "log-traceback";
    return "";
  };

  const tabs = [
    { id: "all", label: "All" },
    { id: "console", label: "Console" },
  ];

  return (
    <div className="logs-view">
      <div className="logs-header">
        <h2 className="logs-title">Logs</h2>
        <div className="logs-controls">
          <div className="logs-tabs">
            {tabs.map((type) => (
              <button
                key={type.id}
                type="button"
                className={`logs-tab ${logType === type.id ? "active" : ""}`}
                onClick={() => setLogType(type.id)}
              >
                {type.label}
              </button>
            ))}
          </div>
          <div className="logs-actions">
            <input
              type="text"
              className="logs-search"
              placeholder="Search logs…"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              maxLength={200}
            />
            <button type="button" className="btn-refresh-logs" onClick={() => void loadLogs()}>
              Refresh
            </button>
            <button type="button" className="btn-copy-logs" onClick={handleCopy}>
              Copy
            </button>
          </div>
        </div>
      </div>
      <div className="logs-content-wrap">
        <div className="logs-content">
          {loading ? (
            <div className="log-line log-empty">Loading...</div>
          ) : filteredLogs.length === 0 ? (
            <div className="log-line log-empty">No logs available</div>
          ) : (
            filteredLogs.map((line, index) => (
              <div key={index} className={`log-line ${getLogClass(line)}`}>
                {line}
              </div>
            ))
          )}
        </div>
      </div>
    </div>
  );
}

export default LogsPage;