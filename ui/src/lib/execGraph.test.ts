import { describe, expect, it } from "vitest";

import {
  emptyExecutionGraph,
  execGraphReducer,
  type ExecutionGraphState,
} from "./execGraph";

type Container = {
  kind: "plan" | "planstep" | "phase" | "subtask";
  title: string;
  status: string;
  children: string[];
  index?: number;
  total?: number;
};

type Tool = {
  kind: "tool";
  title: string;
  status: string;
  summary: string;
  parentId: string;
};

const container = (s: ExecutionGraphState, id: string): Container =>
  s.nodes.get(id) as unknown as Container;

const toolNode = (s: ExecutionGraphState, id: string): Tool =>
  s.nodes.get(id) as unknown as Tool;

function drive(actions: Parameters<typeof execGraphReducer>[1][]): ExecutionGraphState {
  return actions.reduce(execGraphReducer, emptyExecutionGraph);
}

const tool = (
  id: string,
  partial: Partial<{ name: string; status: "running" | "done" | "error"; summary: string }> = {},
) => ({
  type: "tool" as const,
  id,
  name: partial.name ?? "read_file",
  status: partial.status ?? "done",
  summary: partial.summary ?? "",
});

describe("execGraphReducer", () => {
  it("reset clears prior state while keeping plan title", () => {
    const s = drive([
      tool("t1"),
      { type: "reset", planRootTitle: "Fix auth" },
    ]);
    expect(s.nodes.size).toBe(0);
    expect(s.edges.length).toBe(0);
    expect(s.planRootTitle).toBe("Fix auth");
  });

  it("flat tools nest under a root task node", () => {
    const s = drive([tool("t1"), tool("t2")]);
    expect(s.rootId).toBe("root");
    const root = container(s, "root");
    expect(root.kind).toBe("phase");
    expect(root.title).toBe("Task");
    expect(root.children).toEqual(["t1", "t2"]);
    // Sibling tools connected by a sequence edge.
    expect(
      s.edges.some((e) => e.kind === "sequence" && e.from === "t1" && e.to === "t2"),
    ).toBe(true);
  });

  it("plan step creates a root plan node and nests under it", () => {
    const s = drive([
      {
        type: "planStep",
        planId: "p1",
        itemIndex: 0,
        title: "Investigate",
        status: "in_progress",
      },
    ]);
    const root = container(s, "root");
    expect(root.kind).toBe("plan");
    expect(root.children).toEqual(["planstep-p1-0"]);
    const step = container(s, "planstep-p1-0");
    expect(step.kind).toBe("planstep");
    expect(step.status).toBe("running");
  });

  it("subtasks nest under the active plan step", () => {
    const s = drive([
      {
        type: "planStep",
        planId: "p1",
        itemIndex: 0,
        title: "Plan",
        status: "in_progress",
      },
      { type: "subtask", index: 1, total: 2, title: "impl", status: "running" },
      tool("t1"),
    ]);
    const step = container(s, "planstep-p1-0");
    expect(step.children).toEqual(["subtask-1"]);
    const sub = container(s, "subtask-1");
    expect(sub.kind).toBe("subtask");
    expect(sub.children).toEqual(["t1"]);
    const t = toolNode(s, "t1");
    expect(t.kind).toBe("tool");
    expect(t.parentId).toBe("subtask-1");
  });

  it("consecutive subtasks get a sequence edge", () => {
    const s = drive([
      {
        type: "planStep",
        planId: "p1",
        itemIndex: 0,
        title: "Plan",
        status: "completed",
      },
      { type: "subtask", index: 1, total: 2, title: "a", status: "running" },
      { type: "subtask", index: 2, total: 2, title: "b", status: "running" },
    ]);
    expect(
      s.edges.some(
        (e) => e.kind === "sequence" && e.from === "subtask-1" && e.to === "subtask-2",
      ),
    ).toBe(true);
  });

  it("status transitions title nodes to done/failed without duplicating", () => {
    const s = drive([
      {
        type: "planStep",
        planId: "p1",
        itemIndex: 0,
        title: "Plan",
        status: "in_progress",
      },
      {
        type: "planStep",
        planId: "p1",
        itemIndex: 0,
        title: "Plan",
        status: "completed",
      },
      tool("t1", { status: "running" }),
      tool("t1", { status: "error" }),
    ]);
    expect(s.nodes.get("planstep-p1-0")!.status).toBe("done");
    expect(s.nodes.get("t1")!.status).toBe("failed");
    expect(s.nodes.size).toBe(3); // root + planstep + tool, no dupes
  });

  it("tool error maps to failed status", () => {
    const s = drive([tool("t1", { status: "error" })]);
    expect(s.nodes.get("t1")!.status).toBe("failed");
  });

  it("subtask failed maps to failed status", () => {
    const s = drive([
      { type: "subtask", index: 1, total: 1, title: "x", status: "failed" },
    ]);
    expect(s.nodes.get("subtask-1")!.status).toBe("failed");
  });

  it("node id stays stable across plan-step item index collisions", () => {
    const s = drive([
      {
        type: "planStep",
        planId: "p1",
        itemIndex: 2,
        title: "Alpha",
        status: "in_progress",
      },
    ]);
    expect(s.nodes.has("planstep-p1-2")).toBe(true);
  });
});
