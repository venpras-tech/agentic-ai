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

export interface GenParams {
  contextSize: number;
  nThreads: number;
  nGpuLayers: number;
  temperature: number;
  topP: number;
  maxTokens: number;
}

export interface RemoteModelConfig {
  provider: string;
  baseUrl: string;
  apiKey: string;
  model: string;
}

export interface RemoteProviderPreset {
  id: string;
  label: string;
  baseUrl: string;
  apiKeyPlaceholder: string;
  apiKeyRequired: boolean;
  defaultModel: string;
  hint: string;
}

export interface ChatMessage {
  role: "user" | "assistant" | "error";
  content: string;
  sessionId?: number;
  tools?: AgentToolEvent[];
}

export interface AgentToolEvent {
  id: string;
  tool: string;
  status: "running" | "done" | "error";
  summary: string;
  startedAt: number;
  durationMs?: number;
  detail?: string;
  output?: string;
}

export interface FileChangedEvent {
  path: string;
  kind: "write" | "diff";
  diff?: string;
}

export interface ToolOutputEvent {
  tool: string;
  stream: "stdout" | "stderr";
  chunk: string;
}

export interface StepStat {
  step: number;
  tokens: number;
  elapsedMs: number;
  toolCalls: number;
}

export interface StepEvent {
  sessionId: number;
  step: StepStat;
}

export interface SubtaskStat {
  index: number;
  total: number;
  title: string;
  status: "running" | "done" | "failed";
}

export interface SubtaskEvent {
  sessionId: number;
  subtask: SubtaskStat;
}

export interface ExecutionAbortedPayload {
  message: string;
  sessionId: number;
  timestampMs: number;
}

export interface InferenceDone {
  totalTokens: number;
  generatedChars: number;
  tokensPerSec: number;
  elapsedMs: number;
  stopReason: string;
}

export interface OpenFile {
  id: string;
  path: string | null;
  name: string;
  content: string;
  saved: boolean;
}

export interface FileNode {
  name: string;
  path: string;
  isDir: boolean;
}

export interface ContextUsage {
  totalTokens: number;
  limit: number;
  threshold: number;
  usedPercent: number;
  evictedTurns: number;
  messageCount: number;
  overflow: boolean;
}

export interface PermissionRequest {
  requestId: string;
  tool: string;
  summary: string;
  timestampMs: number;
}

export interface Skill {
  name: string;
  description: string;
  content: string;
  source: string;
  active: boolean;
}

export interface KnowledgeReport {
  rules: string;
  rulesSources: string[];
  skills: Skill[];
}

export interface PolicyRule {
  tool: string;
  policy: string;
  commandPatterns: string[];
}

export interface PolicySnapshot {
  default: string;
  rules: PolicyRule[];
}
