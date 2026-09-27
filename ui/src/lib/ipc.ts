import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

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
}

export interface StreamInferenceRequest {
  prompt: string;
  maxTokens: number;
  stopWords: string[];
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
  pattern: string;
  decision: string;
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

export interface McpServerConfig {
  name: string;
  bin: string;
  args: string[];
  env: Record<string, string>;
  allowedTools: string[];
  enabled: boolean;
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
  unloadModel: () => tauriInvoke<void>("unload_model"),
  modelStatus: () => tauriInvoke<Record<string, unknown> | null>("model_status"),
  loadedModelPath: () => tauriInvoke<string | null>("loaded_model_path"),
  mcpLoadCatalog: () => tauriInvoke<McpServerConfig[]>("mcp_catalog_load"),
  mcpSaveCatalog: (servers: McpServerConfig[]) =>
    tauriInvoke<void>("mcp_catalog_save", { servers }),
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

  contextStatus: () => tauriInvoke<ContextUsage>("context_status"),
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

  // ---- background tasks ----
  agentRunBackground: (request: AgentTaskRequest) =>
    tauriInvoke<number>("agent_run_background", { request }),
  listBackgroundTasks: () => tauriInvoke<BackgroundTaskInfo[]>("list_background_tasks"),
  abortBackgroundTask: (taskId: string) =>
    tauriInvoke<void>("abort_background_task", { taskId }),

  // ---- RAG attachments ----
  agentAttachFile: (path: string) =>
    tauriInvoke<AttachedFileInfo>("agent_attach_file", { path }),
  agentDetachFile: (path: string) => tauriInvoke<void>("agent_detach_file", { path }),
  agentListAttachments: () => tauriInvoke<AttachedFileInfo[]>("agent_list_attachments"),

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
  hfSearch: (query: string, limit?: number) =>
    tauriInvoke<Array<{ repoId: string; author: string | null; likes: number; downloads: number; files: Array<{ name: string; size: number | null }> }>>("hf_search", { query, limit }),
  hfDownloadModel: (repoId: string, fileName: string) =>
    tauriInvoke<void>("hf_download_model", { repoId, fileName }),
  hfPauseDownload: (repoId: string, fileName: string) =>
    tauriInvoke<void>("hf_pause_download", { repoId, fileName }),
  hfResumeDownload: (repoId: string, fileName: string) =>
    tauriInvoke<void>("hf_resume_download", { repoId, fileName }),
  hfCancelDownload: (repoId: string, fileName: string, deletePart?: boolean) =>
    tauriInvoke<void>("hf_cancel_download", { repoId, fileName, deletePart }),
  hfListActive: () =>
    tauriInvoke<Array<{ repoId: string; fileName: string; paused: boolean }>>("hf_list_active"),
  hfDownloadProgress: (handler: (data: Record<string, unknown>) => void) =>
    listen("hf-download-progress", (e) => handler(e.payload as Record<string, unknown>)),
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
  }) => {
    const events: Array<[string, keyof typeof handlers]> = [
      ["inference-started", "onStarted"],
      ["inference-token", "onToken"],
      ["inference-done", "onDone"],
      ["inference-error", "onError"],
      ["model-loaded", "onModelLoaded"],
      ["model-load-progress", "onLoadProgress"],
      ["agent://tool-event", "onTool"],
      ["execution-aborted", "onAborted"],
      ["agent-step", "onStep"],
      ["agent-subtask", "onSubtask"],
      ["agent://file-changed", "onFileChanged"],
      ["agent://tool-output", "onToolOutput"],
      ["agent://plan-step", "onPlanStep"],
      ["agent://todo-update", "onTodoUpdate"],
      ["agent://bg-task-event", "onBgTask"],
      ["agent://hitl-gate", "onHitlGate"],
      ["agent://permission-request", "onPermission"],
      ["agent://question-request", "onQuestion"],
      ["agent-knowledge", "onKnowledge"],
      ["agent://skills-changed", "onSkillsChanged"],
      ["workspace://file-changed", "onWorkspaceChanged"],
      ["agent://context-trimmed", "onContextTrimmed"],
    ];
    const unlisteners: (() => void)[] = [];
    for (const [evt, handler] of events) {
      if (handlers[handler]) {
        const un = await listen(evt, (e) => handlers[handler]!(e.payload as Record<string, unknown>));
        unlisteners.push(un);
      }
    }
    return () => { for (const un of unlisteners) un(); };
  },
};

/** Subscribe to streamed terminal output. Returns an unsubscribe fn. */
export function subscribeTerminalOutput(
  handler: (e: TerminalOutputEvent) => void,
): Promise<() => void> {
  return listen("agent://terminal-output", (e) =>
    handler(e.payload as TerminalOutputEvent),
  );
}
