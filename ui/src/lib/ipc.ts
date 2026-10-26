import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type EnginePayload = Record<string, unknown>;
type EngineHandler = (data: EnginePayload) => void;
type UnlistenFn = () => void;

type EngineListenerEntry = {
  handlers: EngineHandler[];
  unlisten: UnlistenFn;
};

// We need to support async listen() — use Promise-wrapped entries.
const engineListeners = new Map<string, Promise<EngineListenerEntry>>();

function addEngineListener(event: string, handler: EngineHandler): Promise<() => void> {
  let entryPromise = engineListeners.get(event);
  if (!entryPromise) {
    const handlers: EngineHandler[] = [];
    entryPromise = listen(event, (e) => {
      const payload = (e.payload as EnginePayload) ?? {};
      for (let i = handlers.length - 1; i >= 0; i--) {
        try {
          handlers[i](payload);
        } catch {
          // ignore handler error
        }
      }
    })
      .then((unlisten) => ({ handlers, unlisten }))
      .catch((err) => {
        engineListeners.delete(event);
        throw err;
      });
    engineListeners.set(event, entryPromise);
  }

  return entryPromise.then(({ handlers, unlisten }) => {
    handlers.push(handler);
    let removed = false;
    return () => {
      if (removed) return;
      removed = true;
      const idx = handlers.indexOf(handler);
      if (idx >= 0) handlers.splice(idx, 1);
      if (handlers.length === 0) {
        try {
          unlisten();
        } finally {
          engineListeners.delete(event);
        }
      }
    };
  });
}

export function isTauriRuntime(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !=
      null
  );
}

export function tauriInvoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!isTauriRuntime()) {
    return Promise.reject(
      new Error(`Desktop backend not available. Launch with \`npm run tauri\`.`),
    );
  }
  return invoke<T>(cmd, args);
}

export interface DownloadedModelInfo {
  repoId: string;
  fileName: string;
  path: string;
  sizeBytes: number;
  sha256?: string | null;
}

export interface HfHubFile {
  name: string;
  size: number | null;
  sha256?: string | null;
}

export interface HfHubModel {
  repoId: string;
  author: string | null;
  description?: string | null;
  pipelineTag?: string | null;
  gated?: boolean | null;
  tags?: string[] | null;
  lastModified?: string | null;
  license?: string | null;
  sha?: string | null;
  likes: number;
  downloads: number;
  files: HfHubFile[];
}

export interface HfTokenStatus {
  present: boolean;
  source?: string | null;
  preview?: string | null;
}

export interface HfModelInfo {
  repoId: string;
  author: string | null;
  pipelineTag: string | null;
  gated: boolean;
  tags: string[];
  lastModified: string | null;
  license: string | null;
  sha: string | null;
  likes: number;
  downloads: number;
  files: Array<{
    name: string;
    size: number | null;
    sha256: string | null;
  }>;
}

export interface ChecksumResult {
  verified: boolean;
  expected: string | null;
  actual: string;
  message: string;
}

export interface StreamInferenceRequest {
  prompt: string;
  maxTokens: number;
  stopWords: string[];
  /** Base64 image attachments (vision-capable remote providers). */
  images?: ImageAttachment[];
}

export interface AgentTaskRequest {
  prompt: string;
  maxTokens: number;
  maxSteps: number;
  stopWords: string[];
  planMode?: boolean;
  verify?: boolean;
  decompose?: boolean;
  /** Total output-token ceiling for the whole task; the loop stops early once exceeded. */
  tokenBudget?: number;
  agentMode?: string;
  workflow?: string;
  /** Base64 image attachments (vision-capable remote providers). */
  images?: ImageAttachment[];
}

export interface ImageAttachment {
  /**
   * `data:image/png;base64,...` (or similar). The backend parses the media
   * type from the data URL and uses the remainder as the base64 body, so the
   * prefix must be kept.
   */
  dataUrl: string;
}

export interface ContextUsage {
  totalTokens: number;
  limit: number;
  threshold: number;
  usedPercent: number;
  evictedTurns: number;
  messageCount: number;
  overflow: boolean;
  breakdown?: Record<string, number>;
}

/** Workspace text index summary surfaced by `index_status`/`index_rebuild`. */
export interface IndexStatus {
  indexedFiles: number;
  totalChars: number;
  workspace: string | null;
  lastBuilt: string | null;
}

/** One `path:line` match from `index_search` / `index_semantic_search`. */
export interface IndexSearchHit {
  path: string;
  line: string;
  lineNumber: number;
}

/** One LSP diagnostic for a file (1-based line/column, server-severity string). */
export interface LspDiagnostic {
  path: string;
  line: number;
  column: number;
  severity: string;
  message: string;
  code: string | null;
}

/** Response from `lsp_diagnostics`: which server answered (if any) + findings. */
export interface LspDiagnostics {
  server: string | null;
  diagnostics: LspDiagnostic[];
}

/** One generation step on `agent://run-trace` (per-step tool/token/duration). */
export interface RunTrace {
  sessionId: number;
  stepIndex: number;
  group: string;
  tools: string[];
  durationMs: number;
  tokensIn: number;
  tokensOut: number;
  outcome: string;
  timestampMs: number;
}

export interface LogFileInfo {
  name: string;
  bytes: number;
  modifiedMs: number;
}

/** One JSONL record the frontend persists for a chat turn. `turnId` makes the
 * backend `session_append` idempotent, so a replayed turn is a no-op. */
export interface SessionAppendRecord {
  role: "user" | "assistant" | "error";
  content: string;
  ts?: number;
  done?: Record<string, unknown>;
  turnId?: string;
}

/** A turn as read back by `session_load` / `session_tail`. */
export interface SessionRecord {
  turnId?: string;
  role?: string;
  content?: string;
  ts?: number;
  model?: { name?: string; provider?: string };
  done?: Record<string, unknown>;
}

export interface SessionChatInfo {
  id: string;
  title: string;
  turns: number;
  updatedAtMs: number;
}

export interface SessionProjectInfo {
  key: string;
  name: string;
  chats: SessionChatInfo[];
}

export interface ForkInfo {
  targetChatId: string;
  copied: number;
  forkTurnId: string | null;
}

export interface PolicyRule {
  tool: string;
  policy: string; // "allow" | "ask" | "deny"
  commandPatterns: string[];
}

export interface PathGrant {
  path: string;
  mode: string;
}

/** The four answers the backend accepts for a pending permission request. */
export type PermissionDecision =
  | "allow_once"
  | "allow_session"
  | "always_allow"
  | "deny";

export interface PermissionRequest {
  requestId: string;
  tool: string;
  summary: string;
  timestampMs: number;
  review?: string;
}

export interface PolicySnapshot {
  default: string;
  rules: PolicyRule[];
  autoApprove?: Record<string, boolean>;
  yolo?: boolean;
  pathGrants: PathGrant[];
}

export interface AuditEntry {
  ts: number;
  id: string;
  tool: string;
  summary: string;
  decision: string;
  startedAt?: number;
  latencyMs: number;
  success?: boolean;
  error?: string;
}

/** Lifecycle of one proposed file edit as seen by the UI badges. */
export type EditStatus = "pending" | "applied" | "rejected" | "failed" | "conflict";

/** One proposed file change waiting on (or resolved by) the user review. */
export interface FileEdit {
  id: string;
  /** Workspace-relative path (forward slashes) when inside the workspace, else absolute. */
  path: string;
  /** 1-based inclusive line range the edit targets, when known. */
  lineStart?: number;
  lineEnd?: number;
  /** "patch" | "search_replace" | "full_content". */
  kind: string;
  /** Unified diff of before -> after, for the review pane. */
  diff?: string;
  /** Full pre-change content. */
  before?: string;
  /** Full post-change content, computed but not yet written. */
  after?: string;
  /** Free-form reason/summary supplied by the agent. */
  reason?: string;
  status: EditStatus;
  error?: string;
}

/** Bundle of edits from one tool call, keyed by group id. Carried over
 * `agent://edit-proposed` / `agent://edit-applied` / `agent://edit-failed`. */
export interface EditBatchEvent {
  sessionId: number;
  groupId: string;
  edits: FileEdit[];
  timestampMs: number;
}

/** Payload of the `vencoder-open-editor` window event: an absolute path, or a
 *  path plus a 1-based line to focus in the editor. */
export type OpenEditorTarget = string | { path: string; line?: number };

/** A proposed-edit batch held while the user reviews it. */
export interface PendingEditGroup {
  id: string;
  sessionId: number;
  tool: string;
  edits: FileEdit[];
}

/** Result of a post-edit verify pass (`agent_run_verify` / `agent://verify-result`). */
export interface VerifyRunResult {
  ok: boolean;
  command: string;
  exitCode?: number;
  report: string;
}

/** Pre-apply snapshot of one edited file (restorable via checkpointRestore). */
export interface FileCheckpoint {
  id: string;
  path: string;
  timestampMs: number;
  editId: string;
  /** sha256 of the snapshot content, for conflict checks. */
  contentHash: string;
}

export interface BackgroundTaskInfo {
  id: string;
  sessionId: number;
  label: string;
  status: "running" | "completed" | "error" | "aborted";
  startedAt: number;
  durationMs?: number;
}

export interface AttachedFileInfo {
  path: string;
  bytes: number;
  chunkCount: number;
}

export interface AgentModeInfo {
  name: string;
  description: string;
}

export interface WorkflowInfo {
  name: string;
  description: string;
}

export interface AutocompleteRequest {
  /** Code before the cursor. */
  prefix: string;
  /** Text after the cursor, when the cursor is mid-file. */
  suffix?: string;
  maxTokens?: number;
}

/** Event payload for streamed terminal output. */
export interface TerminalOutputEvent {
  id: string;
  data: string;
  stream: "stdout" | "stderr" | "exit";
  exitCode?: number | null;
}

/** A live terminal session descriptor. */
export interface TerminalInfo {
  id: string;
  cwd: string;
}

export type McpTransport = "stdio" | "http";

export interface McpServerConfig {
  name: string;
  transport: McpTransport;
  bin: string;
  args: string[];
  url?: string | null;
  headers?: Record<string, string>;
  env: Record<string, string>;
  allowedTools: string[];
  enabled: boolean;
}

export interface McpProbeResult {
  name: string;
  ok: boolean;
  error: string | null;
  tools: string[];
  cached: boolean;
}

export interface Skill {
  name: string;
  description: string;
  content: string;
  source: string;
  active: boolean;
  tags: string[];
  globs: string[];
}

export interface KnowledgeReport {
  rules: string;
  rulesSources: string[];
  skills: Skill[];
}

/** Compile-time + runtime capabilities of the bundled llama.cpp build. */
export interface LlamaBuildInfo {
  /** Pinned `llama-cpp-2` bindings version the binary links. */
  bindingsVersion: string;
  /** llama.cpp's own build summary line (may be empty). */
  systemInfo: string;
  supportsGpuOffload: boolean;
  supportsMmap: boolean;
  supportsMlock: boolean;
  deviceCount: number;
  maxDevices: number;
  computeBufferMib: number;
  gpuFeatureEnabled: boolean;
}

/**
 * Model-load parameters the engine is running with. Mirrors the backend
 * `ModelInitParams` (all optional; `undefined`/`null` = runtime default).
 */
export interface LlamaLoadConfig {
  nGpuLayers?: number | null;
  contextSize?: number | null;
  nThreads?: number | null;
  nWorkers?: number | null;
  nBatch?: number | null;
  nUbatch?: number | null;
  unifiedKv?: boolean | null;
  flashAttn?: boolean | null;
  offloadKqv?: boolean | null;
  useMmap?: boolean | null;
  useMlock?: boolean | null;
  ropeFreqBase?: number | null;
  ropeFreqScale?: number | null;
  cacheTypeK?: string | null;
  cacheTypeV?: string | null;
  seed?: number | null;
  maxTokens?: number | null;
  speculative?: string | null;
  contextCheckpoints?: number | null;
  chatTemplate?: string | null;
  reasoningBudget?: string | null;
  llamaArgs?: string | null;
}

/** Snapshot returned by `llama_runtime_info`. */
export interface LlamaRuntimeInfo {
  build: LlamaBuildInfo;
  /** Loaded model metadata (same shape as `model_status`), or null. */
  model: LlamaModelInfo | null;
  loadConfig: LlamaLoadConfig | null;
  workerCount: number | null;
  loadedPath: string | null;
  remote: boolean;
  cacheTypeK: string | null;
  cacheTypeV: string | null;
}

/** Structural mirror of `context/types.ts` `ModelInfo` (avoids a layer import). */
export interface LlamaModelInfo {
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

export interface LlamaBindingsUpdateInfo {
  currentVersion: string;
  latestVersion: string;
  updateAvailable: boolean;
  cratesIoUrl: string;
  upstreamRepoUrl: string;
  note: string;
}

export interface MediaEngineSettings {
  localBinary?: string | null;
  imageModelPath?: string | null;
  videoModelPath?: string | null;
}

export interface MediaEngineStatus {
  cloudReady: boolean;
  localBinaryConfigured: boolean;
  localBinaryExists: boolean;
  imageModelConfigured: boolean;
  videoModelConfigured: boolean;
  message: string;
}

export interface MediaGenerateImageRequest {
  prompt: string;
  width?: number;
  height?: number;
  model?: string;
  mode?: string;
}

export interface MediaGenerateImageResult {
  savedPath: string;
  provider: string;
  revisedPrompt?: string | null;
}

export interface MediaGenerateVideoRequest {
  prompt: string;
  width?: number;
  height?: number;
  durationSecs?: number;
  model?: string;
}

export interface MediaGenerateVideoResult {
  jobId: string;
  status: string;
  message: string;
}

export const api = {
  minimize: () => getCurrentWindow().minimize(),
  toggleMaximize: () => getCurrentWindow().toggleMaximize(),
  close: () => getCurrentWindow().close(),
  startDrag: () => getCurrentWindow().startDragging(),

  pickAndLoadModel: (params: Record<string, unknown>) =>
    tauriInvoke("pick_and_load_model", { params }),
  loadModelFromPath: (path: string, params?: Record<string, unknown>) =>
    tauriInvoke<Record<string, unknown>>("load_model_from_path", { path, params: params ?? {} }),
  configureRemoteModel: (config: Record<string, unknown>) =>
    tauriInvoke("configure_remote_model", { config }),
  listRemoteModels: (config: Record<string, unknown>) =>
    tauriInvoke<string[]>("list_remote_models", { config }),
  remoteConfigLoad: () =>
    tauriInvoke<Record<string, unknown> | null>("remote_config_load"),
  unloadModel: () => tauriInvoke<void>("unload_model"),
  modelStatus: () => tauriInvoke<Record<string, unknown> | null>("model_status"),
  llamaRuntimeInfo: () => tauriInvoke<LlamaRuntimeInfo>("llama_runtime_info"),
  llamaBindingsUpdateCheck: () =>
    tauriInvoke<LlamaBindingsUpdateInfo>("llama_bindings_update_check"),
  mediaEngineSettingsLoad: () => tauriInvoke<MediaEngineSettings>("media_engine_settings_load"),
  mediaEngineSettingsSave: (settings: MediaEngineSettings) =>
    tauriInvoke<void>("media_engine_settings_save", { settings }),
  mediaEngineStatus: () => tauriInvoke<MediaEngineStatus>("media_engine_status"),
  mediaGenerateImage: (request: MediaGenerateImageRequest) =>
    tauriInvoke<MediaGenerateImageResult>("media_generate_image", { request }),
  mediaGenerateVideo: (request: MediaGenerateVideoRequest) =>
    tauriInvoke<MediaGenerateVideoResult>("media_generate_video", { request }),
  loadedModelPath: () => tauriInvoke<string | null>("loaded_model_path"),
  mcpLoadCatalog: () => tauriInvoke<McpServerConfig[]>("mcp_catalog_load"),
  mcpSaveCatalog: (servers: McpServerConfig[]) =>
    tauriInvoke<void>("mcp_catalog_save", { servers }),
  mcpServerProbe: (name: string) =>
    tauriInvoke<McpProbeResult>("mcp_server_probe", { name }),
  knowledgeReport: () => tauriInvoke<KnowledgeReport>("knowledge_report_cmd"),
  knowledgeScan: () => tauriInvoke<KnowledgeReport>("knowledge_scan"),
  skillSetActive: (name: string, active: boolean) =>
    tauriInvoke<KnowledgeReport>("skill_set_active", { name, active }),
  skillInstall: (source: string, global: boolean) =>
    tauriInvoke<KnowledgeReport>("skill_install", { source, global }),
  skillImportDialog: (kind: "file" | "folder", global: boolean) =>
    tauriInvoke<KnowledgeReport | null>("skill_import_dialog", { kind, global }),
  skillInstallSample: (name: string, content: string, global: boolean) =>
    tauriInvoke<KnowledgeReport>("skill_install_sample", { name, content, global }),
  skillUninstall: (name: string) =>
    tauriInvoke<KnowledgeReport>("skill_uninstall", { name }),
  streamInference: (request: StreamInferenceRequest) =>
    tauriInvoke<number>("stream_inference", { request }),
  cancelInference: () => tauriInvoke<void>("cancel_inference"),
  agentRunTask: (request: AgentTaskRequest) =>
    tauriInvoke<number>("agent_run_task", { request }),
  abortAgentExecution: () =>
    tauriInvoke<{ message: string; sessionId: number }>("abort_agent_execution"),
  agentSteer: (text: string) =>
    tauriInvoke<{ id: string; sessionId: number }>("agent_steer", { text }),
  hitlRespond: (requestId: string, approved: boolean) =>
    tauriInvoke<void>("agent_respond_hitl", { requestId, approved }),
  agentSetYolo: (on: boolean) =>
    tauriInvoke<void>("agent_set_yolo", { on }),
  agentPolicySnapshot: () =>
    tauriInvoke<PolicySnapshot>("agent_policy_snapshot"),
  agentPolicySave: (
    defaultPolicy: string,
    rules: PolicyRule[],
    autoApprove?: Record<string, boolean>,
  ) =>
    tauriInvoke<void>("agent_policy_save", {
      default: defaultPolicy,
      rules,
      autoApprove: autoApprove ?? {},
    }),

  contextStatus: () => tauriInvoke<ContextUsage>("context_status"),
  indexStatus: () => tauriInvoke<IndexStatus>("index_status"),
  indexRebuild: (workspace: string) =>
    tauriInvoke<IndexStatus>("index_rebuild", { workspace }),
  indexSearch: (workspace: string, query: string, maxResults?: number) =>
    tauriInvoke<IndexSearchHit[]>("index_search", {
      workspace,
      query,
      maxResults: maxResults ?? null,
    }),
  indexSemanticSearch: (workspace: string, query: string, maxResults?: number) =>
    tauriInvoke<IndexSearchHit[]>("index_semantic_search", {
      workspace,
      query,
      maxResults: maxResults ?? null,
    }),
  startFileWatcher: (path: string) =>
    tauriInvoke<void>("start_file_watcher", { path }),
  stopFileWatcher: () => tauriInvoke<void>("stop_file_watcher"),
  lspDiagnostics: (path: string) => tauriInvoke<LspDiagnostics>("lsp_diagnostics", { path }),
  contextSetSelection: (selection: {
    path: string;
    startLine?: number | null;
    endLine?: number | null;
    content: string;
  }) =>
    tauriInvoke<ContextUsage>("context_set_selection", {
      path: selection.path,
      startLine: selection.startLine ?? null,
      endLine: selection.endLine ?? null,
      content: selection.content,
    }),
  contextPushTurn: (role: string, content: string) =>
    tauriInvoke<ContextUsage>("context_push_turn", { role, content }),
  contextSetSystemPrompt: (content: string) =>
    tauriInvoke<ContextUsage>("context_set_system_prompt", { content }),
  contextSetFileBuffer: (content: string) =>
    tauriInvoke<ContextUsage>("context_set_file_buffer", { content }),

  // ---- session persistence ----
  sessionAppend: (
    project: string,
    record: SessionAppendRecord,
    chatId?: string | null,
  ) => tauriInvoke<void>("session_append", { project, record, chatId }),
  sessionFork: (
    project: string,
    chatId: string,
    forkTurnId: string | null,
    targetChatId: string,
  ) => tauriInvoke<ForkInfo>("session_fork", { project, chatId, forkTurnId, targetChatId }),
  sessionTail: (project: string, chatId?: string | null, limit?: number) =>
    tauriInvoke<SessionRecord[]>("session_tail", { project, chatId, limit }),
  sessionDeleteChat: (project: string, chatId: string) =>
    tauriInvoke<void>("session_delete_chat", { project, chatId }),

  // ---- HITL / policy / audit ----
  agentRespondPermission: (requestId: string, decision: PermissionDecision) =>
    tauriInvoke<void>("agent_respond_permission", { requestId, decision }),
  agentRespondQuestion: (requestId: string, answer: string) =>
    tauriInvoke<void>("agent_respond_question", { requestId, answer }),
  agentGrantPath: (path: string, mode: "read" | "write") =>
    tauriInvoke<void>("agent_grant_path", { path, mode }),
  agentRevokePath: (path: string) => tauriInvoke<void>("agent_revoke_path", { path }),
  agentAuditLog: (limit?: number) => tauriInvoke<AuditEntry[]>("agent_audit_log", { limit }),

  // ---- edit review / checkpoints ----
  listPendingEdits: () => tauriInvoke<PendingEditGroup[]>("agent_list_pending_edits"),
  applyEdits: (groupId: string, editIds?: string[]) =>
    tauriInvoke<FileEdit[]>("agent_apply_edits", { groupId, editIds }),
  rejectEdits: (groupId: string, editIds?: string[], reason?: string) =>
    tauriInvoke<FileEdit[]>("agent_reject_edits", { groupId, editIds, reason }),
  /** Resolve conflict-status edits: `overwrite=true` force-writes the proposed
   *  content; `overwrite=false` keeps the disk version and rejects the edit. */
  resolveEditConflict: (groupId: string, overwrite: boolean, editIds?: string[]) =>
    tauriInvoke<FileEdit[]>("agent_resolve_edit_conflict", { groupId, overwrite, editIds }),
  runVerify: (files?: string[]) =>
    tauriInvoke<VerifyRunResult>("agent_run_verify", { files }),
  checkpointList: () => tauriInvoke<FileCheckpoint[]>("agent_checkpoint_list"),
  checkpointRestore: (ids?: string[]) =>
    tauriInvoke<number>("agent_checkpoint_restore", { ids }),

  // ---- background tasks ----
  agentRunBackground: (request: AgentTaskRequest) =>
    tauriInvoke<number>("agent_run_background", { request }),
  listBackgroundTasks: () => tauriInvoke<BackgroundTaskInfo[]>("list_background_tasks"),
  abortBackgroundTask: (taskId: string) =>
    tauriInvoke<void>("abort_background_task", { taskId }),
  backgroundTaskOutput: (taskId: string, tailLines?: number) =>
    tauriInvoke<string[]>("background_task_output", { taskId, tailLines }),

  // ---- RAG attachments ----
  agentAttachFile: (path: string) =>
    tauriInvoke<AttachedFileInfo>("agent_attach_file", { path }),
  agentDetachFile: (path: string) => tauriInvoke<void>("agent_detach_file", { path }),
  agentListAttachments: () => tauriInvoke<AttachedFileInfo[]>("agent_list_attachments"),
  pickTextFile: () => tauriInvoke<string | null>("pick_text_file"),

  // ---- editor autocomplete ----
  // Backend is complete and routed through the `autocomplete` provider role,
  // but there is no inline-completion UI yet: `ui/src/components/editor` is a
  // plain <textarea>, not Monaco, so there is no ghost-text layer to render
  // into. Deferred until the editor gets a real completion surface — the
  // wrapper is kept so the UI side is a local change, not an IPC change.
  autocomplete: (request: AutocompleteRequest) =>
    tauriInvoke<string>("autocomplete_generate", { request }),

  // ---- interactive terminal ----
  terminalSpawn: (cwd?: string) => tauriInvoke<string>("terminal_spawn", { cwd }),
  terminalWrite: (id: string, line: string) =>
    tauriInvoke<void>("terminal_write", { id, line }),
  terminalKill: (id: string) => tauriInvoke<void>("terminal_kill", { id }),
  terminalList: () => tauriInvoke<Array<{ id: string; cwd: string }>>("terminal_list"),

  voiceTranscribeData: (data: number[], ext?: string) =>
    tauriInvoke<string>("voice_transcribe_data", { data, ext }),

  pickWorkspaceFolder: () => tauriInvoke<string | null>("pick_workspace_folder"),
  modelsDirInfo: () =>
    tauriInvoke<{ modelsDir: string; defaultDir: string; custom: boolean }>("models_dir_info"),
  openModelsDir: () => tauriInvoke<string>("open_models_dir"),
  agentSetWorkspace: (root: string) =>
    tauriInvoke<void>("agent_set_workspace", { root }),
  agentGetWorkspaces: () => tauriInvoke<string[]>("agent_get_workspaces"),
  agentAddWorkspace: (root: string) =>
    tauriInvoke<string[]>("agent_add_workspace", { root }),
  agentRemoveWorkspace: (root: string) =>
    tauriInvoke<string[]>("agent_remove_workspace", { root }),
  listDirectory: (root: string, relative: string | null = null) =>
    tauriInvoke<Array<{ name: string; path: string; isDir: boolean }>>("list_directory", { root, relative }),
  readTextFile: (path: string) =>
    tauriInvoke<{ path: string; content: string }>("read_text_file", { path }),
  writeTextFile: (path: string, content: string) =>
    tauriInvoke<void>("write_text_file", { path, content }),
  pathExists: (path: string) =>
    tauriInvoke<boolean>("path_exists", { path }),

  modesLoad: () => tauriInvoke<AgentModeInfo[]>("agent_modes"),
  workflowsLoad: () => tauriInvoke<WorkflowInfo[]>("agent_workflows"),
  workflowEnforceTools: (name: string, allowedTools: string[]) =>
    tauriInvoke<void>("workflow_enforce_tools", { name, allowedTools }),
  listDownloadedModels: () =>
    tauriInvoke<DownloadedModelInfo[]>("list_downloaded_models"),
  deleteDownloadedModel: (repoId: string, fileName: string) =>
    tauriInvoke<{ path: string; freedBytes: number; removedRepoDir: boolean }>(
      "delete_downloaded_model",
      { repoId, fileName },
    ),
  hfSearch: (query: string, limit?: number, offset?: number, sort?: string) =>
    tauriInvoke<HfHubModel[]>("hf_search", { query, limit, offset, sort }),
  hfModelInfo: (repoId: string) => tauriInvoke<HfModelInfo>("hf_model_info", { repoId }),
  hfVerifyModel: (repoId: string, fileName: string) =>
    tauriInvoke<ChecksumResult>("hf_verify_model", { repoId, fileName }),
  hfTokenStatus: () => tauriInvoke<HfTokenStatus>("hf_token_status"),
  hfTokenSave: (token: string) => tauriInvoke<void>("hf_token_save", { token }),
  hfTokenClear: () => tauriInvoke<void>("hf_token_clear"),
  hfDownloadModel: (repoId: string, fileName: string, revision?: string | null) =>
    tauriInvoke<void>("hf_download_model", { repoId, fileName, revision: revision ?? null }),
  hfPauseDownload: (repoId: string, fileName: string) =>
    tauriInvoke<void>("hf_pause_download", { repoId, fileName }),
  hfResumeDownload: (repoId: string, fileName: string) =>
    tauriInvoke<void>("hf_resume_download", { repoId, fileName }),
  hfCancelDownload: (repoId: string, fileName: string, deletePart?: boolean) =>
    tauriInvoke<void>("hf_cancel_download", { repoId, fileName, deletePart }),
  hfListActive: () =>
    tauriInvoke<Array<{ repoId: string; fileName: string; paused: boolean }>>("hf_list_active"),
  hfDownloadProgress: (handler: (data: Record<string, unknown>) => void) =>
    addEngineListener("hf-download-progress", handler),
  systemInfo: () =>
    tauriInvoke<{
      os: string;
      cpu: string;
      cores: number;
      totalRamGb: number;
      availableRamGb: number;
      totalSwapGb: number;
      diskFreeGb: number;
      diskTotalGb: number;
      gpu: { name: string; vramGb: number } | null;
      gpuEnabled: boolean;
    }>("system_info"),
  openUrl: (url: string) =>
    invoke("plugin:opener|open_url", { url }),
  consoleHistory: () => tauriInvoke<string[]>("console_history"),
  logDir: () => tauriInvoke<string | null>("log_dir"),
  logFiles: () => tauriInvoke<LogFileInfo[]>("log_files"),
  logRead: (name: string, maxLines?: number) =>
    tauriInvoke<string[]>("log_read", { name, maxLines }),
  logOpenDir: () => tauriInvoke<void>("log_open_dir"),
  logClear: () => tauriInvoke<number>("log_clear"),

  providersList: () => tauriInvoke<Array<Record<string, unknown>>>("providers_list"),
  providersUpsert: (provider: Record<string, unknown>) =>
    tauriInvoke<string>("providers_upsert", { provider }),
  providersRemove: (id: string) => tauriInvoke<boolean>("providers_remove", { id }),
  providersSetRole: (role: string, providerId: string) =>
    tauriInvoke<void>("providers_set_role", { role, providerId }),
  providersClearRole: (role: string) => tauriInvoke<void>("providers_clear_role", { role }),
  providersRoute: (role: string) =>
    tauriInvoke<Record<string, unknown> | null>("providers_route", { role }),

  apiServerStatus: () =>
    tauriInvoke<{ running: boolean; port: number | null }>("api_server_status"),
  apiServerStart: (port?: number) =>
    tauriInvoke<{ running: boolean; port: number | null }>("api_server_start", { port }),
  apiServerStop: () =>
    tauriInvoke<{ running: boolean; port: number | null }>("api_server_stop"),

  settingsLoad: () => tauriInvoke<Record<string, unknown>>("settings_load"),
  settingsSave: (settings: Record<string, unknown>) =>
    tauriInvoke<void>("settings_save", { settings }),
  localModelConfigsLoad: () =>
    tauriInvoke<Record<string, Record<string, unknown>>>("local_model_configs_load"),
  saveLocalModelConfig: (path: string | null, config: Record<string, unknown>) =>
    tauriInvoke<void>("local_model_config_save", { path, config }),

  sessionProjects: () => tauriInvoke<SessionProjectInfo[]>("session_projects"),
  sessionLoad: (project: string, chatId?: string | null) =>
    tauriInvoke<SessionRecord[]>("session_load", { project, chatId }),

  smokeActive: () => tauriInvoke<boolean>("smoke_active"),
  smokeBootOk: () => tauriInvoke<void>("smoke_boot_ok"),

  subscribeEngineEvents: async (handlers: {
    onToken?: (data: Record<string, unknown>) => void;
    onDone?: (data: Record<string, unknown>) => void;
    onError?: (data: Record<string, unknown>) => void;
    onStarted?: (data: Record<string, unknown>) => void;
    onPromptEval?: (data: Record<string, unknown>) => void;
    onModelLoaded?: (data: Record<string, unknown>) => void;
    onLoadProgress?: (data: Record<string, unknown>) => void;
    onTool?: (data: Record<string, unknown>) => void;
    onAborted?: (data: Record<string, unknown>) => void;
    onStep?: (data: Record<string, unknown>) => void;
    onSubtask?: (data: Record<string, unknown>) => void;
    onFileChanged?: (data: Record<string, unknown>) => void;
    onToolOutput?: (data: Record<string, unknown>) => void;
    onPlanStep?: (data: Record<string, unknown>) => void;
    onTodoUpdate?: (data: Record<string, unknown>) => void;
    onBgTask?: (data: Record<string, unknown>) => void;
    onHitlGate?: (data: Record<string, unknown>) => void;
    onPermission?: (data: Record<string, unknown>) => void;
    onQuestion?: (data: Record<string, unknown>) => void;
    onKnowledge?: (data: Record<string, unknown>) => void;
    onSkillsChanged?: (data: Record<string, unknown>) => void;
    onWorkspaceChanged?: (data: Record<string, unknown>) => void;
    onContextTrimmed?: (data: Record<string, unknown>) => void;
    onThinkingUpdated?: (data: Record<string, unknown>) => void;
    onThinkingSnapshot?: (data: Record<string, unknown>) => void;
    onEditApplied?: (data: Record<string, unknown>) => void;
    onEditFailed?: (data: Record<string, unknown>) => void;
    onRunTrace?: (data: Record<string, unknown>) => void;
    onEditProposed?: (data: Record<string, unknown>) => void;
    onIndexProgress?: (data: Record<string, unknown>) => void;
    onVerifyResult?: (data: Record<string, unknown>) => void;
    onSubagentRun?: (data: Record<string, unknown>) => void;
    onHook?: (data: Record<string, unknown>) => void;
  }) => {
    const events: Array<[string, keyof typeof handlers]> = [
      ["inference-started", "onStarted"],
      ["inference-prompt-eval", "onPromptEval"],
      ["inference-token", "onToken"],
      ["inference-done", "onDone"],
      ["inference-error", "onError"],
      ["model-loaded", "onModelLoaded"],
      ["model-load-progress", "onLoadProgress"],
      ["agent://tool-event", "onTool"],
      ["agent://aborted", "onAborted"],
      ["agent://step", "onStep"],
      ["agent://subtask", "onSubtask"],
      ["workspace://file-changed", "onFileChanged"],
      ["agent://tool-output", "onToolOutput"],
      ["agent://plan-step", "onPlanStep"],
      ["agent://todo-update", "onTodoUpdate"],
      ["agent://bg-task-event", "onBgTask"],
      ["agent://hitl-gate", "onHitlGate"],
      ["agent://permission-request", "onPermission"],
      ["agent://question-request", "onQuestion"],
      ["agent://knowledge", "onKnowledge"],
      ["agent://skills-changed", "onSkillsChanged"],
      ["agent://workspace-changed", "onWorkspaceChanged"],
      ["agent://context-trimmed", "onContextTrimmed"],
      ["agent://thinking-updated", "onThinkingUpdated"],
      ["agent://thinking-snapshot", "onThinkingSnapshot"],
      ["agent://edit-applied", "onEditApplied"],
      ["agent://edit-failed", "onEditFailed"],
      ["agent://run-trace", "onRunTrace"],
      ["agent://edit-proposed", "onEditProposed"],
      ["index://progress", "onIndexProgress"],
      ["agent://verify-result", "onVerifyResult"],
      ["agent://subagent-run", "onSubagentRun"],
      ["agent://hook", "onHook"],
    ];
    const unlisteners: Array<() => void> = [];
    try {
      for (const [evt, handler] of events) {
        if (handlers[handler]) {
          const un = await addEngineListener(evt, (payload) => {
            const h = handlers[handler];
            if (h) {
              h(payload);
            }
          });
          unlisteners.push(un);
        }
      }
    } catch (err) {
      for (let i = unlisteners.length - 1; i >= 0; i--) {
        try {
          unlisteners[i]();
        } catch {}
      }
      throw err;
    }
    return () => {
      for (let i = unlisteners.length - 1; i >= 0; i--) {
        try {
          unlisteners[i]();
        } catch {}
      }
    };
  },
};

/** Subscribe to streamed terminal output. Returns an unsubscribe fn. */
export function subscribeTerminalOutput(
  handler: (e: TerminalOutputEvent) => void,
): Promise<() => void> {
  return addEngineListener("agent://terminal-output", (payload) =>
    handler(payload as unknown as TerminalOutputEvent),
  );
}
