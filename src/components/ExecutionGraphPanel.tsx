import {
  type ExecutionGraphState,
  type GraphNode,
  type GraphStatus,
} from "../lib/execGraph";

interface ExecutionGraphPanelProps {
  state: ExecutionGraphState;
  visible: boolean;
  /** Draggable height in px (matches the console/terminal strip). */
  height?: number;
  onClear: () => void;
}

const STATUS_DOT: Record<GraphStatus, string> = {
  pending: "bg-zinc-400",
  running: "bg-cyan-500 animate-pulse",
  done: "bg-emerald-500",
  failed: "bg-red-500",
};

const LEGEND: { status: GraphStatus; label: string; cls: string }[] = [
  { status: "running", label: "running", cls: "bg-cyan-500/15 text-cyan-600" },
  { status: "done", label: "done", cls: "bg-emerald-500/15 text-emerald-600" },
  { status: "failed", label: "failed", cls: "bg-red-500/15 text-red-600" },
  { status: "pending", label: "pending", cls: "bg-zinc-500/15 text-zinc-500" },
];

const KIND_BADGE: Record<GraphNode["kind"], string> = {
  plan: "bg-primary/10 text-primary",
  planstep: "bg-violet-500/15 text-violet-600",
  phase: "bg-sky-500/15 text-sky-600",
  subtask: "bg-amber-500/15 text-amber-700",
  tool: "bg-zinc-500/15 text-zinc-500",
};

function NodeRow({ node, currentNode }: { node: GraphNode; currentNode: string | null }) {
  const isCurrent = node.id === currentNode;
  const badge = KIND_BADGE[node.kind];
  const count =
    node.kind === "subtask" && node.total != null
      ? `${node.index}/${node.total}`
      : node.kind === "planstep"
        ? String((node.index ?? 0) + 1)
        : null;
  return (
    <div
      className={`flex items-start gap-2 rounded px-1.5 py-0.5 ${
        isCurrent ? "bg-accent/10" : ""
      }`}
    >
      <span
        className={`mt-[3px] h-2 w-2 shrink-0 rounded-full ${STATUS_DOT[node.status]}`}
      />
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <span
            className={`shrink-0 rounded px-1 py-px text-[9px] font-semibold uppercase tracking-wide ${badge}`}
          >
            {node.kind}
          </span>
          {count && (
            <span className="shrink-0 text-[9px] font-mono text-zinc-400">{count}</span>
          )}
          <span
            className={`truncate text-[11px] font-medium ${
              isCurrent ? "text-accent" : "text-ink"
            }`}
          >
            {node.title}
          </span>
        </div>
        {node.kind === "tool" && node.summary ? (
          <p className="mt-0.5 truncate pl-[14px] text-[10px] text-zinc-500">
            {node.summary}
          </p>
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
        <div className="ml-[5px] border-l border-border pl-3">
          {children.map((c) => (
            <Tree key={c} id={c} state={state} />
          ))}
        </div>
      )}
    </div>
  );
}

export default function ExecutionGraphPanel({
  state,
  visible,
  height,
  onClear,
}: ExecutionGraphPanelProps) {
  if (!visible) return null;

  const nodes = [...state.nodes.values()];
  const toolCount = nodes.filter((n) => n.kind === "tool").length;
  const containerCount = nodes.filter(
    (n) => n.kind !== "tool",
  ).length;

  return (
    <div
      className="flex shrink-0 select-text flex-col border-t border-border bg-editor"
      style={height != null ? { height } : undefined}
    >
      <div className="flex items-center justify-between border-b border-border px-3 py-1">
        <span className="text-[10px] font-semibold uppercase tracking-wider text-zinc-500">
          Execution Graph
        </span>
        <div className="flex items-center gap-2.5">
          <span className="text-[9px] text-zinc-400">
            {containerCount} nodes · {toolCount} tool calls
          </span>
          <div className="flex items-center gap-1.5">
            {LEGEND.map((l) => (
              <span
                key={l.status}
                className={`flex items-center gap-1 rounded px-1.5 py-px text-[9px] ${l.cls}`}
              >
                <span className={`h-1.5 w-1.5 rounded-full ${STATUS_DOT[l.status]}`} />
                {l.label}
              </span>
            ))}
          </div>
          <button
            onClick={onClear}
            className="rounded px-1.5 py-0.5 text-[10px] text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
          >
            Clear
          </button>
        </div>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-1.5">
        {state.rootId == null ? (
          <p className="py-2 text-[11px] text-zinc-400">
            No run yet — start an agentic task to see its plan, subtasks and tool calls
            as a live graph.
          </p>
        ) : (
          <Tree id={state.rootId} state={state} />
        )}
      </div>
    </div>
  );
}
