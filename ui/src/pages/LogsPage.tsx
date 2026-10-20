import { useState, useEffect, useCallback } from "react";
import { api, type LogFileInfo } from "../lib/ipc";
import "./LogsPage.css";

/** Sources are matched on the `[LLM]` / `[BE]` / `[UI]` tag the backend writes
 *  into every line — the same tags used to colour the output. */
type Source = "all" | "LLM" | "BE" | "UI";
const SOURCES: { id: Source; label: string }[] = [
  { id: "all", label: "All" },
  { id: "LLM", label: "LLM" },
  { id: "BE", label: "Backend" },
  { id: "UI", label: "UI" },
];

const LINE_CAP = 5000;

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}

function LogsPage() {
  const [source, setSource] = useState<Source>("all");
  const [files, setFiles] = useState<LogFileInfo[]>([]);
  const [selected, setSelected] = useState<string>("");
  const [logDir, setLogDir] = useState("");
  const [lines, setLines] = useState<string[]>([]);
  const [searchQuery, setSearchQuery] = useState("");
  const [loading, setLoading] = useState(false);
  const [notice, setNotice] = useState("");

  const loadFiles = useCallback(async (): Promise<string> => {
    const [dir, list] = await Promise.all([
      api.logDir().catch(() => null),
      api.logFiles().catch(() => [] as LogFileInfo[]),
    ]);
    setLogDir(dir ?? "");
    setFiles(list ?? []);
    return list?.[0]?.name ?? "";
  }, []);

  const loadLogs = useCallback(
    async (name?: string) => {
      setLoading(true);
      setNotice("");
      try {
        const target = name ?? (selected || (await loadFiles()));
        setSelected(target);
        if (!target) {
          setLines([]);
          setNotice("No log files found yet.");
        } else {
          setLines((await api.logRead(target, LINE_CAP)) ?? []);
        }
      } catch (e) {
        setLines([]);
        setNotice(e instanceof Error ? e.message : String(e));
      } finally {
        setLoading(false);
      }
    },
    [selected, loadFiles],
  );

  useEffect(() => {
    void loadLogs();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice(""), 4000);
    return () => clearTimeout(t);
  }, [notice]);

  const handleOpenFolder = async () => {
    try {
      await api.logOpenDir();
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
    }
  };

  const handleClear = async () => {
    setLoading(true);
    try {
      await api.logClear();
      await loadLogs(selected || undefined);
      setNotice("Logs cleared.");
    } catch (e) {
      setNotice(e instanceof Error ? e.message : String(e));
      setLoading(false);
    }
  };

  const handleDownload = () => {
    const body = filteredLogs.join("\n");
    const name = (selected || "ai-editor").replace(/\.log$/, "");
    const blob = new Blob([body], { type: "text/plain;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${name}${source === "all" ? "" : `-${source}`}.log`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
    setNotice(`Downloaded ${filteredLogs.length} line(s).`);
  };

  const bySource = (line: string) =>
    source === "all" ? true : line.includes(`[${source}]`);

  const filteredLogs = lines.filter(
    (line) =>
      bySource(line) &&
      (!searchQuery || line.toLowerCase().includes(searchQuery.toLowerCase())),
  );

  const counts = {
    LLM: lines.filter((l) => l.includes("[LLM]")).length,
    BE: lines.filter((l) => l.includes("[BE]")).length,
    UI: lines.filter((l) => l.includes("[UI]")).length,
  };

  const getLogClass = (line: string) => {
    if (line.includes("ERROR")) return "log-error";
    if (line.includes("WARN")) return "log-warning";
    if (line.includes("INFO")) return "log-info";
    if (line.includes("DEBUG")) return "log-debug";
    if (line.includes("Traceback")) return "log-traceback";
    return "";
  };

  return (
    <div className="logs-view">
      <div className="logs-header">
        <h2 className="logs-title">Logs</h2>
        <div className="logs-controls">
          <div className="logs-tabs">
            {SOURCES.map((s) => (
              <button
                key={s.id}
                type="button"
                className={`logs-tab ${source === s.id ? "active" : ""}`}
                onClick={() => setSource(s.id)}
              >
                {s.label}
                {s.id !== "all" && counts[s.id] > 0 && (
                  <span className="logs-tab-count">{counts[s.id]}</span>
                )}
              </button>
            ))}
          </div>
          <div className="logs-actions">
            <select
              className="logs-file"
              value={selected}
              onChange={(e) => void loadLogs(e.target.value)}
              title="Log file"
            >
              {files.length === 0 && <option value="">no log files</option>}
              {files.map((f) => (
                <option key={f.name} value={f.name}>
                  {f.name} ({formatBytes(f.bytes)})
                </option>
              ))}
            </select>
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
            <button type="button" className="btn-open-logs" onClick={() => void handleOpenFolder()}>
              Open folder
            </button>
            <button type="button" className="btn-download-logs" onClick={handleDownload}>
              Download
            </button>
            <button type="button" className="btn-clear-logs" onClick={() => void handleClear()}>
              Clear
            </button>
          </div>
        </div>
        <div className="logs-path" title={logDir}>
          {logDir || "logs folder unavailable"}
          {filteredLogs.length > 0 && (
            <span className="logs-path-count">
              {" "}
              · {filteredLogs.length}/{lines.length} lines
            </span>
          )}
        </div>
        {notice && <div className="logs-notice">{notice}</div>}
      </div>
      <div className="logs-content-wrap">
        <div className="logs-content">
          {loading ? (
            <div className="log-line log-empty">Loading…</div>
          ) : filteredLogs.length === 0 ? (
            <div className="log-line log-empty">
              {lines.length === 0 ? "No log lines available" : "No lines match this filter"}
            </div>
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