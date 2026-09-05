import type { PlanStepEvent, SubtaskStat } from "../types";

/**
 * Live execution-graph model for agentic runs.
 *
 * The engine emits flat, timestamped events (`agent://plan-step`, step,
 * subtask and tool events). This reducer folds those into a small DAG:
 *
 *   root (plan | task)
 *    └─ plan step            (one per `agent://plan-step` item)
 *        ├─ subtask          (one per subtask `running` event)
 *        │   ├─ tool
 *        │   └─ tool  ← sequence edge between sibling tools
 *        └─ subtask  ← sequence edge between sibling subtasks
 *
 * or, for flat (non-decomposed) runs:
 *   root (task)
 *    └─ phase "Execute"
 *        └─ tool
 *
 * Nodes are folded entirely from the event stream, so the reducer is pure and
 * unit-testable and independent of any graph-layout library.
 */

export type GraphStatus = "pending" | "running" | "done" | "failed";

type GraphContainerNode = {
  id: string;
  kind: "plan" | "phase" | "planstep" | "subtask";
  title: string;
  status: GraphStatus;
  children: string[];
  /** planstep: item index; subtask: 1-based position. */
  index?: number;
  total?: number;
};

type GraphToolNode = {
  id: string;
  kind: "tool";
  title: string;
  status: GraphStatus;
  summary: string;
  parentId: string;
};

export type GraphNode = GraphContainerNode | GraphToolNode;

export interface ExecutionGraphEdge {
  from: string;
  to: string;
  kind: "nest" | "sequence";
}

export interface ExecutionGraphState {
  nodes: Map<string, GraphNode>;
  edges: ExecutionGraphEdge[];
  rootId: string | null;
  /** Accent the switch currently receiving events. */
  currentId: string | null;
  /** Container node that receives the next tool (subtask / phase / plan step). */
  containerId: string | null;
  planRootTitle?: string;
}

export const emptyExecutionGraph: ExecutionGraphState = {
  nodes: new Map(),
  edges: [],
  rootId: null,
  currentId: null,
  containerId: null,
};

export type ExecutionGraphAction =
  | { type: "reset"; planRootTitle?: string }
  | {
      type: "planStep";
      planId: string;
      itemIndex: number;
      title: string;
      status: PlanStepEvent["status"];
    }
  | {
      type: "subtask";
      index: number;
      total: number;
      title: string;
      status: SubtaskStat["status"];
    }
  | { type: "step"; group: string }
  | {
      type: "tool";
      id: string;
      name: string;
      status: "running" | "done" | "error";
      summary: string;
    };

type IncomingStatus =
  | PlanStepEvent["status"]
  | SubtaskStat["status"]
  | "done"
  | "error";

function statusOf(status: IncomingStatus): GraphStatus {
  if (status === "running" || status === "in_progress") return "running";
  if (status === "failed" || status === "error") return "failed";
  // "terminal", "completed", "done"
  return "done";
}

interface Cursor {
  /** Latest plan-step container (subtasks and phase steps nest under it). */
  planStepId: string | null;
  /** Container that receives the next tool. */
  containerId: string | null;
}

export function execGraphReducer(
  state: ExecutionGraphState,
  action: ExecutionGraphAction,
): ExecutionGraphState {
  if (action.type === "reset") {
    return {
      nodes: new Map(),
      edges: [],
      rootId: null,
      currentId: null,
      containerId: null,
      planRootTitle: action.planRootTitle,
    };
  }

  const nodes = new Map(state.nodes);
  const edges = [...state.edges];
  // Authoritative previous-sibling per parent (nest parents), for sequence edges.
  const prevSibling: Map<string, string | null> = buildPrevSibling(edges);

  const ensureRoot = (preferred?: "plan" | "phase"): string => {
    if (state.rootId != null) return state.rootId;
    const id = "root";
    nodes.set(id, {
      id,
      kind: preferred ?? (state.planRootTitle != null ? "plan" : "phase"),
      title: preferred === "plan" ? "Plan" : state.planRootTitle ?? "Task",
      status: "done",
      children: [],
    });
    return id;
  };

  const nest = (parentId: string, child: GraphNode): void => {
    const parent = nodes.get(parentId);
    if (!parent || parent.kind === "tool") return;
    parent.children.push(child.id);
    nodes.set(parentId, parent);
    edges.push({ from: parentId, to: child.id, kind: "nest" });
    const prev = prevSibling.get(parentId) ?? null;
    if (prev && prev !== child.id) {
      edges.push({ from: prev, to: child.id, kind: "sequence" });
    }
    prevSibling.set(parentId, child.id);
    nodes.set(child.id, child);
  };

  let cursor: Cursor = {
    planStepId: lastOfKind(nodes, "planstep"),
    containerId: state.containerId ?? state.rootId,
  };

  switch (action.type) {
    case "planStep": {
      const id = `planstep-${action.planId}-${action.itemIndex}`;
      const status = statusOf(action.status);
      const rootId = ensureRoot("plan");
      const existing = nodes.get(id);
      if (existing && existing.kind !== "tool") {
        nodes.set(id, { ...existing, status });
      } else {
        nest(rootId, {
          id,
          kind: "planstep",
          title: `Plan · ${action.title}`,
          status,
          children: [],
          index: action.itemIndex,
        });
      }
      cursor = { planStepId: id, containerId: id };
      return {
        ...state,
        nodes,
        edges,
        rootId,
        currentId: status === "running" ? id : state.currentId,
        containerId: id,
      };
    }
    case "subtask": {
      const id = `subtask-${action.index}`;
      const status = statusOf(action.status);
      const rootId = ensureRoot();
      const parentId = cursor.planStepId ?? rootId;
      const existing = nodes.get(id);
      if (existing && existing.kind !== "tool") {
        nodes.set(id, { ...existing, status });
      } else {
        nest(parentId, {
          id,
          kind: "subtask",
          title: `Subtask ${action.index}/${action.total} · ${action.title}`,
          status,
          children: [],
          index: action.index,
          total: action.total,
        });
      }
      cursor = { ...cursor, containerId: id };
      return {
        ...state,
        nodes,
        edges,
        rootId,
        currentId: status === "running" ? id : state.currentId,
        containerId: id,
      };
    }
    case "step": {
      // Subtask groups are handled by `subtask` events; only phase containers
      // (flat runs) are materialized here.
      if (/^Subtask /.test(action.group)) {
        return { ...state, nodes, edges };
      }
      const rootId = ensureRoot();
      const id = `phase-${slugify(action.group) || "phase"}`;
      const parentId = cursor.planStepId ?? rootId;
      if (!nodes.has(id)) {
        nest(parentId, {
          id,
          kind: "phase",
          title: action.group,
          status: "done",
          children: [],
        });
      }
      return {
        ...state,
        nodes,
        edges,
        rootId,
        currentId: cursor.planStepId ?? id,
        containerId: cursor.planStepId ?? id,
      };
    }
    case "tool": {
      const id = action.id;
      const status = statusOf(action.status);
      const rootId = ensureRoot();
      const parentId = cursor.containerId ?? cursor.planStepId ?? rootId;
      const existing = nodes.get(id);
      if (existing && existing.kind === "tool") {
        nodes.set(id, {
          ...existing,
          status,
          summary: action.summary || existing.summary,
        });
      } else {
        nest(parentId, {
          id,
          kind: "tool",
          title: action.name.replace(/_/g, " "),
          summary: action.summary,
          status,
          parentId,
        });
      }
      return {
        ...state,
        nodes,
        edges,
        rootId,
        currentId: status === "running" ? id : state.currentId,
        containerId: state.containerId,
      };
    }
    default:
      return state;
  }
}

/** Map each parent to its most recently nested child (for sequence edges). */
function buildPrevSibling(edges: ExecutionGraphEdge[]): Map<string, string | null> {
  const result = new Map<string, string | null>();
  for (const e of edges) {
    if (e.kind === "nest") result.set(e.from, e.to);
  }
  return result;
}

function lastOfKind(nodes: Map<string, GraphNode>, kind: GraphNode["kind"]): string | null {
  let last: string | null = null;
  for (const n of nodes.values()) if (n.kind === kind) last = n.id;
  return last;
}

function slugify(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
