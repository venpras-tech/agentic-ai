/**
 * Chat response lifecycle state machine.
 *
 * Pure reducer + view derivation — no React, no Tauri imports — so the full
 * transition graph is unit-testable. `App.tsx` drives it with events from
 * `useEngineEvents`; `StatusIndicator.tsx` renders the derived view.
 *
 * Lifecycle: idle → sending → thinking → streaming ⇄ working → complete | error
 *
 *   sending    invoke issued, backend has not acknowledged yet
 *   thinking   `inference-started` received, waiting for the first token
 *   streaming  token deltas flowing
 *   working    agent activity between generations (steps, tools, subtasks,
 *              permission dialogs) while tokens are not streaming
 *   complete   terminal success (auto-hidden by the component after a beat)
 *   error      terminal failure; message surfaces inline
 */

export type ChatPhase =
  | "idle"
  | "sending"
  | "thinking"
  | "streaming"
  | "working"
  | "complete"
  | "error";

export interface ChatStatus {
  phase: ChatPhase;
  sessionId: number | null;
  /** Short human label for the current activity ("step 2 · Execute"). */
  label: string | null;
  /** performance.now() of the last transition, for elapsed/stale display. */
  sinceMs: number;
  /** Characters streamed in the current turn. */
  chars: number;
  error?: string;
}

export const initialChatStatus: ChatStatus = {
  phase: "idle",
  sessionId: null,
  label: null,
  sinceMs: 0,
  chars: 0,
};

/** ms without any event before the UI hints that something may be stuck. */
export const STALE_AFTER_MS = 45_000;
/** ms after submit before the backend ack is considered slow. */
export const ACK_SLOW_AFTER_MS = 10_000;

export type ChatStatusEvent =
  | { type: "submit"; at: number }
  | { type: "started"; sessionId: number; at: number }
  | { type: "token"; sessionId: number; len: number; at: number }
  | {
      type: "step";
      sessionId: number;
      step: number;
      group?: string;
      at: number;
    }
  | {
      type: "tool";
      sessionId?: number;
      tool: string;
      status: string;
      summary?: string;
      at: number;
    }
  | {
      type: "subtask";
      index: number;
      total: number;
      title: string;
      status: string;
      at: number;
    }
  | { type: "permission"; tool: string; at: number }
  | { type: "done"; sessionId: number; at: number }
  | { type: "error"; message: string; at: number }
  | { type: "reset"; at: number };

const ACTIVE: ReadonlySet<ChatPhase> = new Set([
  "sending",
  "thinking",
  "streaming",
  "working",
]);

export function isActivePhase(phase: ChatPhase): boolean {
  return ACTIVE.has(phase);
}

/** Terminal phases stop ticking but stay visible briefly. */
export function isTerminalPhase(phase: ChatPhase): boolean {
  return phase === "complete" || phase === "error" || phase === "idle";
}

export function reduceChatStatus(
  state: ChatStatus,
  event: ChatStatusEvent,
): ChatStatus {
  switch (event.type) {
    case "submit":
      if (isActivePhase(state.phase)) return state;
      return {
        phase: "sending",
        sessionId: null,
        label: null,
        sinceMs: event.at,
        chars: 0,
      };
    case "started":
      // A stray started for an older session must not hijack a live turn.
      if (
        isActivePhase(state.phase) &&
        state.sessionId != null &&
        state.sessionId !== event.sessionId
      ) {
        return state;
      }
      return {
        ...state,
        phase: "thinking",
        sessionId: event.sessionId,
        label: null,
        sinceMs: event.at,
      };
    case "token": {
      if (!isActivePhase(state.phase)) return state;
      if (state.sessionId != null && state.sessionId !== event.sessionId)
        return state;
      return {
        ...state,
        phase: "streaming",
        sessionId: event.sessionId,
        chars: state.chars + event.len,
        sinceMs: event.at,
      };
    }
    case "step":
      if (isActivePhase(state.phase)) {
        return {
          ...state,
          phase: "working",
          sessionId: event.sessionId,
          label: event.group
            ? `step ${event.step} · ${event.group}`
            : `step ${event.step}`,
          sinceMs: event.at,
        };
      }
      return state;
    case "tool": {
      if (!isActivePhase(state.phase)) return state;
      if (event.status === "running") {
        return {
          ...state,
          phase: "working",
          label: event.summary ?? event.tool,
          sinceMs: event.at,
        };
      }
      // done/error on a tool keeps the turn active but drops its label.
      return state.phase === "working" && state.label === (event.summary ?? event.tool)
        ? { ...state, label: null, sinceMs: event.at }
        : state;
    }
    case "subtask":
      if (!isActivePhase(state.phase)) return state;
      if (event.status === "running") {
        return {
          ...state,
          phase: "working",
          label: `subtask ${event.index}/${event.total} · ${event.title}`,
          sinceMs: event.at,
        };
      }
      return state.label === `subtask ${event.index}/${event.total} · ${event.title}`
        ? { ...state, label: null, sinceMs: event.at }
        : state;
    case "permission":
      if (!isActivePhase(state.phase)) return state;
      return {
        ...state,
        phase: "working",
        label: `waiting for approval · ${event.tool}`,
        sinceMs: event.at,
      };
    case "done":
      if (!isActivePhase(state.phase)) return state;
      if (state.sessionId != null && state.sessionId !== event.sessionId)
        return state;
      return {
        phase: "complete",
        sessionId: event.sessionId,
        label: null,
        sinceMs: event.at,
        chars: state.chars,
      };
    case "error":
      if (isTerminalPhase(state.phase)) return state;
      return {
        phase: "error",
        sessionId: state.sessionId,
        label: null,
        sinceMs: event.at,
        chars: state.chars,
        error: event.message,
      };
    case "reset":
      return { ...initialChatStatus };
  }
}

/** Everything the indicator needs to render, derived from raw state. */
export interface StatusView {
  phase: ChatPhase;
  label: string;
  /** Full sentence for screen readers. */
  announcement: string;
  /** Turn has been quiet long enough to hint at the console. */
  stale: boolean;
}

const BASE_LABELS: Record<ChatPhase, string> = {
  idle: "",
  sending: "Sending…",
  thinking: "Warming up the model…",
  streaming: "Streaming…",
  working: "Working…",
  complete: "Done",
  error: "Failed",
};

export function statusView(state: ChatStatus, now: number): StatusView {
  const base = state.label ?? BASE_LABELS[state.phase];
  let stale = false;
  if (state.phase === "sending") {
    stale = now - state.sinceMs > ACK_SLOW_AFTER_MS;
  } else if (ACTIVE.has(state.phase)) {
    stale = now - state.sinceMs > STALE_AFTER_MS;
  }
  const label = stale
    ? `${base.replace(/…$/, "")} — still running, see Console (Ctrl+\`)`
    : base;
  const verb =
    state.phase === "error"
      ? "failed"
      : state.phase === "complete"
        ? "completed"
        : "in progress";
  return {
    phase: state.phase,
    label,
    announcement: `Request ${verb}${label ? `: ${label}` : ""}`,
    stale,
  };
}
