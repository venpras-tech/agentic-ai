import { create } from "zustand";
import { emptyExecutionGraph, execGraphReducer } from "../lib/execGraph";
import type { ExecutionGraphAction, ExecutionGraphState } from "../lib/execGraph";

interface ExecGraphState {
  graph: ExecutionGraphState;
  dispatch: (action: ExecutionGraphAction) => void;
}

export const useExecGraphStore = create<ExecGraphState>((set) => ({
  graph: emptyExecutionGraph,
  dispatch: (action) =>
    set((s) => ({
      graph: execGraphReducer(s.graph, action),
    })),
}));