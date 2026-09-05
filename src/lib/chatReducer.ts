import type {
  AgentToolEvent,
  ChatMessage,
  FileChangedEvent,
  LedgerEntry,
  StepTimelineStep,
} from "../types";

/**
 * Reducer for the chat transcript state (message list + per-turn ledger).
 * Extracted from the App.tsx monolith so every message/ledger transition the
 * engine-event handlers perform is a pure, unit-testable transform — the UI
 * "Messages = State" contract of the session model.
 */
export interface ChatState {
  messages: ChatMessage[];
  ledger: LedgerEntry[];
}

export const emptyChat: ChatState = { messages: [], ledger: [] };

export type ChatAction =
  | { type: "reset" }
  | { type: "clearMessages" }
  | { type: "replaceAll"; messages: ChatMessage[] }
  | { type: "push"; message: ChatMessage }
  | { type: "mergeById"; sessionId: number; patch: Partial<ChatMessage> }
  | { type: "tool"; sessionId: number; tool: AgentToolEvent }
  | { type: "appendStep"; sessionId: number; step: StepTimelineStep }
  | { type: "appendPlanStep"; sessionId: number; group: string }
  | { type: "appendToolOutput"; sessionId: number; chunk: string }
  | { type: "appendDiff"; sessionId: number; diff: FileChangedEvent }
  | {
      type: "diffResolved";
      messageIndex: number;
      diffIndex: number;
      status: "accepted" | "rejected";
    }
  | { type: "ledgerSet"; entry: LedgerEntry }
  | { type: "ledgerTool"; sessionId: number; label: string }
  | { type: "ledgerTokens"; sessionId: number; label: string; tokens: number };

/** Cap for live terminal/test output kept per tool card (matches ChatPanel UI). */
const MAX_TOOL_OUTPUT = 4000;

function mapBySessionId(
  messages: ChatMessage[],
  sessionId: number,
  fn: (m: ChatMessage) => ChatMessage,
): ChatMessage[] {
  return messages.map((m) => (m.sessionId === sessionId ? fn(m) : m));
}

function upsertLedger(ledger: LedgerEntry[], entry: LedgerEntry): LedgerEntry[] {
  const idx = ledger.findIndex((l) => l.sessionId === entry.sessionId);
  return idx >= 0
    ? ledger.map((l, i) => (i === idx ? entry : l))
    : [...ledger, entry];
}

export function chatReducer(state: ChatState, action: ChatAction): ChatState {
  switch (action.type) {
    case "reset":
      return emptyChat;
    case "clearMessages":
      return { ...state, messages: [] };
    case "replaceAll":
      return { ...state, messages: action.messages };
    case "push":
      return { ...state, messages: [...state.messages, action.message] };
    case "mergeById":
      return {
        ...state,
        messages: mapBySessionId(state.messages, action.sessionId, (m) => ({
          ...m,
          ...action.patch,
        })),
      };
    case "tool": {
      const { tool } = action;
      return {
        ...state,
        messages: mapBySessionId(state.messages, action.sessionId, (m) => {
          const tools = m.tools ?? [];
          const existing = tools.some((t) => t.id === tool.id);
          return {
            ...m,
            tools: existing
              ? tools.map((t) =>
                  t.id === tool.id ? { ...t, ...tool, output: t.output } : t,
                )
              : [...tools, tool],
          };
        }),
      };
    }
    case "appendStep":
      return {
        ...state,
        messages: mapBySessionId(state.messages, action.sessionId, (m) => ({
          ...m,
          steps: [...(m.steps ?? []), action.step],
        })),
      };
    case "appendPlanStep":
      return {
        ...state,
        messages: mapBySessionId(state.messages, action.sessionId, (m) => {
          const existing = m.steps ?? [];
          return {
            ...m,
            steps: [
              ...existing,
              {
                step: existing.length + 1,
                group: action.group,
                tokens: 0,
                elapsedMs: 0,
                toolCalls: 0,
              },
            ],
          };
        }),
      };
    case "appendToolOutput":
      return {
        ...state,
        messages: mapBySessionId(state.messages, action.sessionId, (m) => {
          const idx = (m.tools ?? []).findIndex(
            (t) =>
              t.status === "running" &&
              (t.tool === "execute_terminal_command" || t.tool === "run_tests"),
          );
          if (idx < 0) return m;
          const next = (m.tools ?? []).map((t, i) => {
            if (i !== idx) return t;
            const merged = `${t.output ?? ""}${action.chunk}\n`;
            return {
              ...t,
              output:
                merged.length > MAX_TOOL_OUTPUT
                  ? merged.slice(-MAX_TOOL_OUTPUT)
                  : merged,
            };
          });
          return { ...m, tools: next };
        }),
      };
    case "appendDiff":
      return {
        ...state,
        messages: mapBySessionId(state.messages, action.sessionId, (m) => ({
          ...m,
          diffs: [...(m.diffs ?? []), action.diff],
        })),
      };
    case "diffResolved": {
      const { messageIndex, diffIndex, status } = action;
      const target = state.messages[messageIndex];
      if (!target || !target.diffs || !target.diffs[diffIndex]) return state;
      if (target.diffs[diffIndex].resolved === status) return state;
      return {
        ...state,
        messages: state.messages.map((m, i) =>
          i !== messageIndex
            ? m
            : {
                ...m,
                diffs: m.diffs!.map((d, di) =>
                  di !== diffIndex ? d : { ...d, resolved: status },
                ),
              },
        ),
      };
    }
    case "ledgerSet":
      return { ...state, ledger: upsertLedger(state.ledger, action.entry) };
    case "ledgerTool": {
      const idx = state.ledger.findIndex((l) => l.sessionId === action.sessionId);
      const entry: LedgerEntry = {
        sessionId: action.sessionId,
        label: action.label,
        tokens: idx >= 0 ? state.ledger[idx].tokens : 0,
        toolCalls: (idx >= 0 ? state.ledger[idx].toolCalls : 0) + 1,
        elapsedMs: idx >= 0 ? state.ledger[idx].elapsedMs : 0,
      };
      return { ...state, ledger: upsertLedger(state.ledger, entry) };
    }
    case "ledgerTokens": {
      const idx = state.ledger.findIndex((l) => l.sessionId === action.sessionId);
      const entry: LedgerEntry = {
        sessionId: action.sessionId,
        label: action.label,
        tokens: (idx >= 0 ? state.ledger[idx].tokens : 0) + action.tokens,
        toolCalls: idx >= 0 ? state.ledger[idx].toolCalls : 0,
        elapsedMs: idx >= 0 ? state.ledger[idx].elapsedMs : 0,
      };
      return { ...state, ledger: upsertLedger(state.ledger, entry) };
    }
  }
}