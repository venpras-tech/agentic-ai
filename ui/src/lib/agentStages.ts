/**
 * Derives a single "what is the model doing right now" label from the agent
 * event stream so the chat can show progress before the first token lands.
 *
 * Kept as a pure reducer (no React, no Tauri) so it can be unit tested the same
 * way as `appendRunTrace` / `mergeEditBatch`.
 */

export type AgentStage =
  | "idle"
  | "queued"
  | "thinking"
  | "responding"
  | "working"
  | "done"
  | "error";

export interface AgentStageState {
  stage: AgentStage;
  label: string;
  detail?: string;
  /** Prompt size in tokens while evaluating it, for the "reading context" row. */
  promptTokens?: number;
  /** Timestamp (ms) the current stage began, for elapsed-time display. */
  since: number;
  /** Completed agent steps so far, for "step N" progress. */
  steps: number;
}

export type AgentStageEvent =
  | { type: "queued" }
  | { type: "started" }
  | { type: "prompt"; tokens: number }
  | { type: "token" }
  | { type: "step" }
  | { type: "tool"; tool?: string; status?: string }
  | { type: "done" }
  | { type: "error" };

export const IDLE_STAGE: AgentStageState = { stage: "idle", label: "", since: 0, steps: 0 };

export const QUEUED_STAGE: AgentStageState = {
  stage: "queued",
  label: "Sending request…",
  since: 0,
  steps: 0,
};

/** Stages worth showing a spinner for. `done`/`error`/`idle` are silent: the
 *  finished bubble (or its error fence) is the feedback at that point. */
export function isStageVisible(stage: AgentStageState): boolean {
  return stage.stage === "queued" || stage.stage === "thinking"
    || stage.stage === "responding" || stage.stage === "working";
}

/**
 * The submit → `inference-started` gap has no event of its own, so a run that
 * is flagged as processing but has not reported a stage yet is still "sending".
 */
export function stageOrQueued(stage: AgentStageState, processing: boolean): AgentStageState {
  return stage.stage === "idle" && processing ? QUEUED_STAGE : stage;
}

const FINISHED: AgentStage[] = ["idle", "done", "error"];

export function reduceAgentStage(
  prev: AgentStageState,
  event: AgentStageEvent,
  now = 0,
): AgentStageState {
  switch (event.type) {
    case "queued":
      return { stage: "queued", label: "Sending request…", since: now, steps: 0 };
    case "started":
      return { stage: "thinking", label: "Thinking…", since: now, steps: 0 };
    case "prompt":
      // Prompt evaluation is the long pole before the first token. Keep the
      // stage's original `since` so an in-flight elapsed timer does not reset
      // when the prompt-size detail arrives a beat after "started".
      if (FINISHED.includes(prev.stage)) return prev;
      return {
        ...prev,
        stage: "thinking",
        label: prev.stage === "thinking" ? prev.label : "Thinking…",
        promptTokens: event.tokens > 0 ? event.tokens : undefined,
        since: prev.since || now,
        steps: 0,
      };
    case "token":
      // Tokens keep streaming for the whole reply: do not restart the timer.
      if (prev.stage === "responding") return prev;
      return {
        ...prev,
        stage: "responding",
        label: "Responding…",
        detail: undefined,
        promptTokens: undefined,
        since: now,
      };
    case "step":
      if (FINISHED.includes(prev.stage)) return prev;
      return {
        ...prev,
        steps: prev.steps + 1,
        label: prev.stage === "responding" ? prev.label : "Working…",
        detail: undefined,
      };
    case "tool": {
      if (FINISHED.includes(prev.stage)) return prev;
      const name = (event.tool ?? "").trim();
      const status = (event.status ?? "").toLowerCase();
      if (status === "running" || status === "pending" || status === "started") {
        return {
          ...prev,
          stage: "working",
          label: name ? `Running ${name}…` : "Running a tool…",
          detail: name || undefined,
          since: now,
        };
      }
      // A finished tool hands control back to the model, which usually needs a
      // moment before the next token — stay on "Working…" instead of claiming
      // it is responding.
      return { ...prev, stage: "working", label: "Working…", detail: undefined, since: now };
    }
    case "done":
      return { ...prev, stage: "done", label: "Done", detail: undefined, since: now };
    case "error":
      return { ...prev, stage: "error", label: "Stopped with an error", detail: undefined, since: now };
    default:
      return prev;
  }
}