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
  /** Repetition penalty (1 = off); suppresses degenerate token loops. */
  repeatPenalty: number;
  maxTokens: number;
}

export interface RemoteModelConfig {
  provider: string;
  baseUrl: string;
  apiKey: string;
  model: string;
}

/** A user-defined agent mode from `.ai/modes/*.md` (camelCase mirror of the
 *  Rust `subagent::Mode`). An empty `allowedTools` means unrestricted. */
export interface AgentMode {
  name: string;
  description: string;
  systemPrompt: string;
  allowedTools: string[];
  allowedGlobs?: string[];
  modelOverride?: string;
}

/** A reusable recipe in `.ai/workflows/*.md`, invoked via `/name`. */
export interface Workflow {
  name: string;
  description: string;
  systemPrompt: string;
  allowedTools: string[];
}

/** Persisted `{project, chatId}` pointer (survives app restarts). */
export interface LastChatPointer {
  project: string;
  chatId: string | null;
}

/**
 * Typed mirror of the Rust `AppSettings` struct (`{app_data}/settings.json`).
 * Fields are optional because the file is written by both the model lifecycle
 * (`modelPath`) and the frontend, and unknown keys survive a load→save round
 * trip via the catch-all.
 */
export interface AppSettingsRecord {
  modelPath?: string;
  recentModels?: string[];
  params?: Partial<GenParams>;
  remote?: RemoteModelConfig;
  lastWorkspace?: string;
  lastWorkspaces?: string[];
  lastChat?: LastChatPointer;
  /** Preserved verbatim on load→save round trips (legacy/unknown keys). */
  [key: string]: unknown;
}

export type ProviderRole =
  | "planner"
  | "editor"
  | "autocomplete"
  | "embed";

export type ProviderKind =
  | "local"
  | "openai"
  | "ollama"
  | "openrouter"
  | "anthropic"
  | "google"
  | "lmstudio"
  | "deepseek"
  | "xai"
  | "groq"
  | "mistral"
  | "custom";

export interface ProviderConfig {
  id: string;
  name: string;
  kind: ProviderKind;
  baseUrl?: string;
  apiKey?: string;
  model: string;
  roles: ProviderRole[];
  contextSize?: number;
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
  /**
   * Client-generated UUID for this turn; both the user and assistant halves of
   * a turn share it. Stored in the JSONL record as `turnId` so the backend can
   * dedupe replayed `sessionAppend` writes (idempotent turns).
   */
  turnId?: string;
  sessionId?: number;
  /** Wall-clock completion time (ms epoch); persisted for exports. */
  ts?: number;
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
  /** Live terminal/test output — accumulated client-side from
   *  `tool-output` events (`execute_terminal_command`, `run_tests`). */
  output?: string;
  /** Owning agent session (from the backend) — pins the event to its turn. */
  sessionId: number;
  /**
   * Client-side anchor: character offset into the turn's streamed text at
   * which this call fired. Used to interleave tool cards INLINE between
   * paragraphs instead of stacking them after the finished text.
   */
  atChar?: number;
}

export interface FileChangedEvent {
  path: string;
  kind: "write" | "diff";
  diff?: string;
  /** Pre-change file content (for undo/revert). */
  before?: string;
  /** Per-diff resolution, set client-side via the chat reducer
   *  (`chatReducer` "diffResolved"). Absent while still pending. */
  resolved?: "accepted" | "rejected";
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
  /** Model the sub-task is running on (first-class subagents; decompose may omit). */
  model?: string;
  /** Running/completed duration in ms (0 while just started). */
  elapsedMs?: number;
  /** Tool currently executing; absent while generating or between tools. */
  tool?: string;
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
  outcome: "completed" | "failed" | "interrupted" | "error";
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
  decision:
    | "allow"
    | "deny"
    | "granted"
    | "granted-session"
    | "granted-always"
    | "declined"
    | "timed-out"
    | "aborted";
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

/** An attached image carried as a base64 `data:` URL for vision-capable remote
 * providers. Not supported by the local llama.cpp path. */
export interface ImageAttachment {
  dataUrl: string;
  alt?: string;
}

export interface ContextUsage {
  totalTokens: number;
  limit: number;
  threshold: number;
  usedPercent: number;
  evictedTurns: number;
  messageCount: number;
  overflow: boolean;
  /** Per-category token split surfaced in the status bar. */
  breakdown?: ContextBreakdown;
}

export interface ContextBreakdown {
  system: number;
  file: number;
  rules: number;
  skills: number;
  memory: number;
  otherPinned: number;
  turns: number;
}

export interface PermissionRequest {
  requestId: string;
  tool: string;
  summary: string;
  timestampMs: number;
  /** Independent LLM shell-approval review (Bionic §3.3), when available. */
  review?: string;
}

/** Blocking question from the agent (`ask_question`, P1-9). */
export interface QuestionRequest {
  requestId: string;
  question: string;
  /** Preset answer buttons; may be empty (free-text only). */
  choices: string[];
  timestampMs: number;
}

export interface Skill {
  name: string;
  description: string;
  content: string;
  source: string;
  active: boolean;
  /** User-defined tags surfaced in the knowledge panel (empty when none). */
  tags?: string[];
  /** Glob patterns this skill applies to (matched against active-file paths). */
  globs?: string[];
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
  /** "allow" | "ask" | "deny" — the lenient loader preserves unknown values, but
   *  this is the documented contract (see `.ai/policy.json`). */
  policy: "allow" | "ask" | "deny";
  commandPatterns: string[];
}

export interface PolicySnapshot {
  /** Fallback verdict for tools with no explicit rule. */
  default: "ask" | "allow" | "deny";
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
  /** User-given name when this checkpoint was created with `git_checkpoint(..., name)` /
   *  `git_checkpoints(..., name)`; resolved from `.ai/checkpoints.json`. */
  name?: string;
}

/** A user-named snapshot (`.ai/checkpoints.json`), keyed by commit hash. */
export interface NamedCheckpoint {
  hash: string;
  name: string;
  timeMs: number;
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

/** Info about a background task (P2-12). */
export interface BackgroundTaskInfo {
  id: string;
  sessionId: number;
  label: string;
  /** "running" | "completed" | "error" | "aborted". */
  status: "running" | "completed" | "error" | "aborted";
  startedAt: number;
  durationMs?: number;
}

/** Background task lifecycle event (agent://bg-task-event). */
export interface BackgroundTaskEvent {
  taskId: string;
  sessionId: number;
  label: string;
  /** "started" | "completed" | "error" | "aborted". */
  status: "started" | "completed" | "error" | "aborted";
  detail?: string;
}
