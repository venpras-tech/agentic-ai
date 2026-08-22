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
  /** Per-step telemetry for this turn (plan / subtask / execute phases). */
  steps?: StepTimelineStep[];
  /** File diffs attached to this turn (agent://file-changed), in order. */
  diffs?: FileChangedEvent[];
  /** Turn-lifecycle stats when this assistant turn finished. */
  done?: InferenceDone;
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
  /** Phase label from the orchestrator: "Plan", "Execute" or
   *  "Subtask N/M · title". Used to group steps into a collapsible timeline. */
  group: string;
  tokens: number;
  elapsedMs: number;
  toolCalls: number;
}

/** One step already folded into a chat message's timeline. */
export interface StepTimelineStep {
  step: number;
  group: string;
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
  /** Turn lifecycle outcome: "completed" | "failed" | "interrupted" | "error". */
  outcome: string;
  /** Prompt tokens sent to the model this turn. */
  inputTokens: number;
  /** Tokens generated this turn. */
  outputTokens: number;
  /** Tokens served from a prompt cache (0 for local llama.cpp). */
  cacheReadTokens: number;
  /** Tokens written into the prompt cache. */
  cacheWriteTokens: number;
  /** Reasoning/thinking tokens, when the provider reports them. */
  reasoningTokens: number;
}

/** One tool-decision record from `.ai/audit.jsonl` (camelCase). */
export interface AuditEntry {
  ts: number;
  id: string;
  tool: string;
  summary: string;
  /** "allow" | "deny" | "granted" | "granted-session" | "granted-always" |
   *  "declined" | "timed-out" | "aborted". */
  decision: string;
  startedAt?: number;
  latencyMs: number;
  success: boolean | null;
  error: string | null;
}

/** How the user answered a permission prompt (see PermissionDecision in Rust). */
export type PermissionDecision =
  | "allow_once"
  | "allow_session"
  | "always_allow"
  | "deny";

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

/** One chat under a project (projects/chats sidebar tree). */
export interface SessionChatInfo {
  /** Empty string for the default (legacy) chat. */
  id: string;
  title: string;
  updatedAtMs: number;
  turns: number;
}

/** One project with all of its chats, newest activity first. */
export interface SessionProjectInfo {
  /** Filesystem-safe key; stable across launches. */
  key: string;
  /** Original workspace path when known; use this for API calls. */
  name: string;
  lastActiveMs: number;
  chats: SessionChatInfo[];
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
  /** Independent LLM shell-approval review (Bionic §3.3), when available. */
  review?: string;
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

export interface McpServerConfig {
  name: string;
  bin: string;
  args: string[];
  /** Extra environment variables for the spawned server process. */
  env?: Record<string, string>;
  /** Non-empty: only these tool names are callable (`*` suffix = prefix wildcard). */
  allowedTools?: string[];
  enabled: boolean;
}

export interface HfFile {
  name: string;
  size: number | null;
}

export interface HfModel {
  repoId: string;
  author: string | null;
  likes: number;
  downloads: number;
  files: HfFile[];
}

export interface DownloadedModel {
  repoId: string;
  fileName: string;
  path: string;
  sizeBytes: number;
}

export interface ApiServerStatus {
  running: boolean;
  port: number | null;
}

export interface HubDownloadProgress {
  repoId: string;
  file: string;
  receivedBytes?: number;
  totalBytes?: number | null;
  done?: boolean;
  cancelled?: boolean;
  error?: string;
}

export interface AttachedFileInfo {
  path: string;
  bytes: number;
  chunkCount: number;
}

export interface PolicyRule {
  tool: string;
  policy: string;
  commandPatterns: string[];
}

export interface PolicySnapshot {
  default: string;
  rules: PolicyRule[];
  /** YOLO sub-mode: ROUTINE shell commands skip approval (session-only). */
  yolo?: boolean;
  /** Per-session path grants for paths outside the workspace. */
  pathGrants?: { path: string; mode: "read" | "write" }[];
}

export interface CheckpointInfo {
  hash: string;
  subject: string;
  relative: string;
}

export interface ToolResultInfo {
  success: boolean;
  tool: string;
  summary: string;
  stdout?: string;
  error?: string;
  durationMs: number;
}

/** Aggregate ledger entry for one agentic session. */
export interface LedgerEntry {
  sessionId: number;
  label: string;
  tokens: number;
  toolCalls: number;
  elapsedMs: number;
}

/** Per-plan-item progress event (agent://plan-step). */
export interface PlanStepEvent {
  sessionId: number;
  planId: string;
  itemIndex: number;
  title: string;
  status: "in_progress" | "completed" | "terminal" | "failed";
  error?: string;
}

/** One todo entry (Bionic §3.2 PLANNING). */
export interface TodoItem {
  id: number;
  title: string;
  done: boolean;
}

/** Live todo-list snapshot (agent://todo-update). */
export interface TodoUpdateEvent {
  items: TodoItem[];
  updatedAt: number;
}
