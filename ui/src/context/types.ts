import type { ImageAttachment } from "../lib/ipc";

export interface ActivityLogEntry {
  text: string;
  type: "status" | "error" | "tool" | "index";
}

export interface ChatMessage {
  role: "user" | "assistant" | "error";
  content: string;
  streaming?: boolean;
  ts?: number;
  /** Snapshot of the model/provider that produced this turn (chat display + logs). */
  model?: { name: string; provider: string };
  done?: {
    outcome?: string;
    outputTokens?: number;
    inputTokens?: number;
    tokensPerSec?: number;
    elapsedMs?: number;
    stopReason?: string;
  };
}

export type MessageUpdate = Omit<Partial<ChatMessage>, "content"> & {
  content?: string | ((prev: string) => string);
};

export interface CodeContextSegment {
  path: string;
  startLine?: number;
  endLine?: number;
}

/** Which context chips the user attached for the next agent turn. */
export interface ContextState {
  code: CodeContextSegment[];
  codebase: boolean;
  docs: string[];
  git: { mode: string } | null;
  web: string;
  past_chats: boolean;
  browser: boolean;
  visual: { image: string } | null;
}

export interface ConversationSummary {
  id: string;
  title: string;
  created_at: string;
}

export interface LlmSettings {
  contextSize?: number;
  nThreads?: number;
  nGpuLayers?: number;
  nWorkers?: number;
}

export interface ModelInfo {
  name: string;
  architecture: string;
  nVocab: number;
  nCtxTrain: number;
  nEmbd: number;
  nLayer: number;
  nParams: number;
  sizeBytes: number;
  contextSize: number;
}

export type LastModelSelection =
  | { kind: "local"; name: string; path: string }
  | {
      kind: "remote";
      name: string;
      provider: string;
      providerLabel: string;
      baseUrl: string;
      apiKey: string;
    };

/** The active editor's text, mirrored from EditorPane for agent context. */
export interface ActiveFileBuffer {
  path: string;
  content: string;
}

export const defaultContextState: ContextState = {
  code: [],
  codebase: false,
  docs: [],
  git: null,
  web: "",
  past_chats: false,
  browser: false,
  visual: null,
};

export type { ImageAttachment };
