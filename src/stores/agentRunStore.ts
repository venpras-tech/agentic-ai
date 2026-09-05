import { create } from "zustand";
import type { InferenceDone } from "../types";

export interface SubtaskRun {
  index: number;
  total: number;
  title: string;
  /** Model the sub-task is running on (first-class subagents). */
  model?: string;
  /** Tool currently executing; absent while generating / between tools. */
  tool?: string;
  /** Wall-clock epoch ms when this sub-task began (live elapsed ticking). */
  startedAt?: number;
}

/** A prompt queued while the agent is busy; flushed serially when a turn ends. */
export interface QueuedPrompt {
  text: string;
  opts?: { planMode?: boolean; verify?: boolean; decompose?: boolean };
}

interface AgentRunState {
  /** Backend id of the agentic session currently streaming (null when idle). */
  activeSessionId: number | null;
  /** True while any foreground inference is in flight. */
  isStreaming: boolean;
  /** Result of the most recently completed inference (cleared on new sends). */
  lastDone: InferenceDone | null;
  /** Current decomposition/plan step index being executed. */
  currentStep: number | null;
  /** Current sub-task of a decomposition run (latest event). */
  currentSubtask: SubtaskRun | null;
  /** All sub-tasks still running (first-class `task` batches run in parallel). */
  runningSubtasks: SubtaskRun[];
  /** Prompts submitted while busy, in submission order, run after each turn. */
  queuedPrompts: QueuedPrompt[];
  /** Length of the queue, mirrored for the small "N queued" badge. */
  queuedCount: number;
  setActiveSessionId: (id: number | null) => void;
  setIsStreaming: (v: boolean) => void;
  setLastDone: (d: InferenceDone | null) => void;
  setCurrentStep: (s: number | null) => void;
  setCurrentSubtask: (s: SubtaskRun | null) => void;
  /** Upsert one running sub-task, preserving its start time across refresh events. */
  upsertSubtask: (s: SubtaskRun) => void;
  /** Drop a finished sub-task by (index, total) identity. */
  removeSubtask: (index: number, total: number) => void;
  /** Append a prompt to the busy-queue (updates queuedCount). */
  enqueuePrompt: (p: QueuedPrompt) => void;
  /** Dequeue the next queued prompt (no-op when empty). */
  shiftPrompt: () => QueuedPrompt | undefined;
  /** Reset the run-scoped fields together (start/error/send boundaries). */
  resetRun: () => void;
}

const sameSubtask = (a: SubtaskRun, index: number, total: number) =>
  a.index === index && a.total === total;

export const useAgentRunStore = create<AgentRunState>((set, get) => ({
  activeSessionId: null,
  isStreaming: false,
  lastDone: null,
  currentStep: null,
  currentSubtask: null,
  runningSubtasks: [],
  queuedPrompts: [],
  queuedCount: 0,
  setActiveSessionId: (id) => set({ activeSessionId: id }),
  setIsStreaming: (v) => set({ isStreaming: v }),
  setLastDone: (d) => set({ lastDone: d }),
  setCurrentStep: (s) => set({ currentStep: s }),
  setCurrentSubtask: (s) => set({ currentSubtask: s }),
  upsertSubtask: (s) =>
    set((state) => {
      const prev = state.runningSubtasks.find((r) =>
        sameSubtask(r, s.index, s.total),
      );
      const merged: SubtaskRun =
        prev != null && prev.startedAt != null
          ? { ...s, startedAt: prev.startedAt }
          : s;
      return {
        runningSubtasks: [
          ...state.runningSubtasks.filter(
            (r) => !sameSubtask(r, s.index, s.total),
          ),
          merged,
        ],
      };
    }),
  removeSubtask: (index, total) =>
    set((state) => ({
      runningSubtasks: state.runningSubtasks.filter(
        (r) => !sameSubtask(r, index, total),
      ),
    })),
  resetRun: () =>
    set({
      activeSessionId: null,
      isStreaming: false,
      currentStep: null,
      currentSubtask: null,
      runningSubtasks: [],
    }),
  enqueuePrompt: (p) =>
    set((state) => ({
      queuedPrompts: [...state.queuedPrompts, p],
      queuedCount: state.queuedPrompts.length + 1,
    })),
  shiftPrompt: () => {
    const q = get().queuedPrompts;
    if (q.length === 0) return undefined;
    const [next, ...rest] = q;
    set({ queuedPrompts: rest, queuedCount: rest.length });
    return next;
  },
}));

export function resetAgentRunStoreForTests() {
  useAgentRunStore.setState({
    activeSessionId: null,
    isStreaming: false,
    lastDone: null,
    currentStep: null,
    currentSubtask: null,
    runningSubtasks: [],
    queuedPrompts: [],
    queuedCount: 0,
  });
}

/** Stale-safe read of the run flag for callbacks/event handlers. */
export function runStreaming(): boolean {
  return useAgentRunStore.getState().isStreaming;
}