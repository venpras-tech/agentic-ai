import { useMemo } from "react";
import {
  type SubagentRunState,
  type BackgroundTaskState,
} from "../../hooks/useAgentStream";
import "./runBoard.css";

interface RunBoardProps {
  subagentRuns: SubagentRunState[];
  bgTasks: BackgroundTaskState[];
  onAbortBackground?: (id: string) => void;
  onClose: () => void;
}

interface Card {
  id: string;
  kind: "subagent" | "background";
  title: string;
  status: string;
  detail?: string;
  column: Column;
}

type Column = "running" | "waiting" | "done" | "failed";

const COLUMNS: Array<{ id: Column; label: string }> = [
  { id: "running", label: "Running" },
  { id: "waiting", label: "Waiting" },
  { id: "done", label: "Done" },
  { id: "failed", label: "Failed" },
];

function columnFor(status: string): Column {
  const s = status.toLowerCase();
  if (/(fail|error|abort|cancel|denied)/.test(s)) return "failed";
  if (/(queued|pending|waiting|blocked|paused|review|await)/.test(s)) return "waiting";
  if (/(done|complete|success|finished|ok|applied)/.test(s)) return "done";
  return "running";
}

function normalizeStatus(status: string): string {
  const s = (status || "").toLowerCase();
  if (/(fail|error)/.test(s)) return "failed";
  if (/(abort|cancel)/.test(s)) return "aborted";
  if (/(complete|success|finished|ok)/.test(s)) return "done";
  if (/(queued|pending|waiting)/.test(s)) return "queued";
  if (/(run|active|working)/.test(s)) return "running";
  return s || "running";
}

/** Multiplexed run board (P5.6): a Kanban view over live subagent and
 *  background runs, grouped by lifecycle stage. Read-only mirror of the
 *  timeline data — no extra backend state. */
function RunBoard({ subagentRuns, bgTasks, onAbortBackground, onClose }: RunBoardProps) {
  const cards = useMemo<Card[]>(() => {
    const out: Card[] = [];
    for (const r of subagentRuns) {
      out.push({
        id: `sub-${r.key}`,
        kind: "subagent",
        title: r.profile || "subagent",
        status: normalizeStatus(r.status),
        detail: r.summary || undefined,
        column: columnFor(r.status),
      });
    }
    for (const t of bgTasks) {
      out.push({
        id: `bg-${t.id}`,
        kind: "background",
        title: t.label || t.id,
        status: normalizeStatus(t.status),
        detail: t.detail,
        column: columnFor(t.status),
      });
    }
    return out;
  }, [subagentRuns, bgTasks]);

  const byColumn = useMemo(() => {
    const map: Record<Column, Card[]> = { running: [], waiting: [], done: [], failed: [] };
    for (const c of cards) map[c.column].push(c);
    return map;
  }, [cards]);

  return (
    <div className="run-board" role="region" aria-label="Multiplexed run board">
      <div className="run-board-head">
        <span className="run-board-title">Runs</span>
        <span className="run-board-count">{cards.length}</span>
        <button type="button" className="run-board-close" onClick={onClose} title="Hide run board">
          ✕
        </button>
      </div>
      <div className="run-board-cols">
        {COLUMNS.map((col) => (
          <div key={col.id} className={`run-board-col run-board-col--${col.id}`}>
            <div className="run-board-col-head">
              {col.label}
              <span className="run-board-col-count">{byColumn[col.id].length}</span>
            </div>
            <div className="run-board-cards">
              {byColumn[col.id].length === 0 ? (
                <div className="run-board-empty">—</div>
              ) : (
                byColumn[col.id].map((card) => (
                  <div key={card.id} className={`run-board-card run-board-card--${card.kind}`}>
                    <div className="run-board-card-head">
                      <span className="run-board-card-kind" aria-hidden>
                        {card.kind === "subagent" ? "◈" : "▤"}
                      </span>
                      <span className="run-board-card-title" title={card.title}>
                        {card.title}
                      </span>
                      <span className={`run-board-card-status status--${card.status}`}>
                        {card.status}
                      </span>
                    </div>
                    {card.detail && <p className="run-board-card-detail">{card.detail}</p>}
                    {card.kind === "background" && card.column === "running" && onAbortBackground && (
                      <button
                        type="button"
                        className="run-board-card-abort"
                        onClick={() => onAbortBackground(card.id.replace(/^bg-/, ""))}
                        title="Abort this background task"
                      >
                        abort
                      </button>
                    )}
                  </div>
                ))
              )}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

export default RunBoard;
