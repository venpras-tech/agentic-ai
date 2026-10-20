import type { ExecutionGraphState, GraphNode, GraphStatus } from "../../lib/execGraph";
import "./execGraph.css";

interface ExecutionGraphPanelProps {
  state: ExecutionGraphState;
  visible: boolean;
  onClear: () => void;
  onClose?: () => void;
}

const STATUS_CLASS: Record<GraphStatus, string> = {
  pending: "pending",
  running: "running",
  done: "done",
  failed: "failed",
};

const LEGEND: { status: GraphStatus; label: string }[] = [
  { status: "running", label: "running" },
  { status: "done", label: "done" },
  { status: "failed", label: "failed" },
  { status: "pending", label: "pending" },
];

const KIND_CLASS: Record<GraphNode["kind"], string> = {
  plan: "plan",
  planstep: "planstep",
  phase: "phase",
  subtask: "subtask",
  tool: "tool",
};

function NodeRow({ node, currentNode }: { node: GraphNode; currentNode: string | null }) {
  const isCurrent = node.id === currentNode;
  const count =
    node.kind === "subtask" && node.total != null
      ? `${node.index}/${node.total}`
      : node.kind === "planstep"
        ? String((node.index ?? 0) + 1)
        : null;
  return (
    <div className={`execgraph-row${isCurrent ? " current" : ""}`}>
      <span className={`execgraph-dot ${STATUS_CLASS[node.status]}`} />
      <div className="execgraph-row-body">
        <div className="execgraph-row-head">
          <span className={`execgraph-kind ${KIND_CLASS[node.kind]}`}>{node.kind}</span>
          {count && <span className="execgraph-count">{count}</span>}
          <span className="execgraph-title">{node.title}</span>
        </div>
        {node.kind === "tool" && node.summary ? (
          <p className="execgraph-summary">{node.summary}</p>
        ) : null}
      </div>
    </div>
  );
}

/** Render one node, then (recursively) its nested children with a guide line. */
function Tree({ id, state }: { id: string; state: ExecutionGraphState }) {
  const node = state.nodes.get(id);
  if (!node) return null;
  const children = node.kind === "tool" ? [] : node.children;
  return (
    <div>
      <NodeRow node={node} currentNode={state.currentId} />
      {children.length > 0 && (
        <div className="execgraph-children">
          {children.map((c) => (
            <Tree key={c} id={c} state={state} />
          ))}
        </div>
      )}
    </div>
  );
}

function ExecutionGraphPanel({ state, visible, onClear, onClose }: ExecutionGraphPanelProps) {
  if (!visible) return null;

  const nodes = [...state.nodes.values()];
  const toolCount = nodes.filter((n) => n.kind === "tool").length;
  const containerCount = nodes.filter((n) => n.kind !== "tool").length;

  return (
    <div className="execgraph" role="region" aria-label="Execution graph">
      <div className="execgraph-header">
        <span className="execgraph-heading">Execution Graph</span>
        <div className="execgraph-header-right">
          <span className="execgraph-stats">
            {containerCount} nodes · {toolCount} tool calls
          </span>
          <div className="execgraph-legend">
            {LEGEND.map((l) => (
              <span key={l.status} className="execgraph-legend-item">
                <span className={`execgraph-dot ${STATUS_CLASS[l.status]}`} />
                {l.label}
              </span>
            ))}
          </div>
          <button type="button" className="execgraph-clear" onClick={onClear}>
            Clear
          </button>
          {onClose && (
            <button
              type="button"
              className="execgraph-close"
              onClick={onClose}
              aria-label="Close execution graph"
              title="Close"
            >
              ×
            </button>
          )}
        </div>
      </div>
      <div className="execgraph-body">
        {state.rootId == null ? (
          <p className="execgraph-empty">
            No run yet — start an agentic task to see its plan, subtasks and tool calls as a
            live graph.
          </p>
        ) : (
          <Tree id={state.rootId} state={state} />
        )}
      </div>
    </div>
  );
}

export default ExecutionGraphPanel;
