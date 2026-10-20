import { useCallback, useEffect, useRef, useState } from "react";
import { api } from "../../lib/ipc";
import "./bgTaskOutputDrawer.css";

interface BgTaskOutputDrawerProps {
  taskId: string;
  label: string;
  running: boolean;
  onClose: () => void;
  onAbort?: () => void;
}

/**
 * Output drawer for one background task: tails the backend ring buffer
 * (`background_task_output`) on a 1s poll while the task is running, and
 * offers abort + close. Kept deliberately small — the buffer is capped
 * server-side, so this just renders whatever tail it gets.
 */
function BgTaskOutputDrawer({
  taskId,
  label,
  running,
  onClose,
  onAbort,
}: BgTaskOutputDrawerProps) {
  const [lines, setLines] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const bodyRef = useRef<HTMLPreElement>(null);

  const load = useCallback(async () => {
    try {
      const out = await api.backgroundTaskOutput(taskId, 200);
      setLines(out);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [taskId]);

  useEffect(() => {
    void load();
    if (!running) return;
    const timer = window.setInterval(() => void load(), 1000);
    return () => window.clearInterval(timer);
  }, [load, running]);

  useEffect(() => {
    const el = bodyRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines]);

  return (
    <div className="bg-output-drawer">
      <div className="bg-output-header">
        <span className="bg-output-title" title={label}>
          {label}
        </span>
        {onAbort && running && (
          <button type="button" className="bg-output-action" onClick={onAbort}>
            Abort
          </button>
        )}
        <button
          type="button"
          className="bg-output-action"
          onClick={onClose}
          title="Close output"
        >
          ×
        </button>
      </div>
      <pre className="bg-output-body" ref={bodyRef}>
        {error ? error : lines.length > 0 ? lines.join("\n") : "(no output yet)"}
      </pre>
    </div>
  );
}

export default BgTaskOutputDrawer;
