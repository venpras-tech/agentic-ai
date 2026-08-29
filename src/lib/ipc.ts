import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type {
  ApiServerStatus,
  AttachedFileInfo,
  AuditEntry,
  BackgroundTaskInfo,
  ContextUsage,
  DownloadedModel,
  GenParams,
  FileNode,
  HfModel,
  KnowledgeReport,
  McpServerConfig,
  ModelInfo,
  PolicySnapshot,
  ProviderConfig,
  ProviderRole,
  RemoteModelConfig,
  SessionProjectInfo,
  CheckpointInfo,
  ToolResultInfo,
} from "../types";
import { parseEvent, EVT_CONTEXT_TRIMMED, type EngineHandlers } from "./events";

/**
 * True when running inside the Tauri desktop shell. The `@tauri-apps/api`
 * bridge reads `window.__TAURI_INTERNALS__`, which only exists in the Tauri
 * webview — opening `npm run dev` in a plain browser has no backend, and any
 * `invoke()` would otherwise fail with a cryptic
 * "Cannot read properties of undefined (reading 'invoke')".
 */
export function isTauriRuntime(): boolean {
  return (
    typeof window !== "undefined" &&
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !=
      null
  );
}

/** `invoke` wrapper with a clear error when the desktop bridge is absent. */
export function tauriInvoke<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!isTauriRuntime()) {
    return Promise.reject(
      new Error(
        `The desktop backend is not available in this browser tab. ` +
          `Launch the app with \`npm run tauri:dev\` (not \`npm run dev\`) so the ` +
          `Tauri shell can connect \`${cmd}\`.`,
      ),
    );
  }
  return invokeWithRetry<T>(cmd, args, 0);
}

export function tauriInvokeWrite<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!isTauriRuntime()) {
    return Promise.reject(
      new Error(
        `The desktop backend is not available in this browser tab. ` +
          `Launch the app with \`npm run tauri:dev\` (not \`npm run dev\`) so the ` +
          `Tauri shell can connect \`${cmd}\`.`,
      ),
    );
  }
  return invoke<T>(cmd, args).catch((err) => {
    if (isTransientError(err)) {
      throw new Error(
        `Write command \`${cmd}\` may not have been applied: ${err instanceof Error ? err.message : String(err)}`,
      );
    }
    throw err;
  });
}

const MAX_RETRIES = 2;
const RETRY_DELAYS = [200, 500];

function invokeWithRetry<T>(
  cmd: string,
  args: Record<string, unknown> | undefined,
  attempt: number,
): Promise<T> {
  return invoke<T>(cmd, args).catch((err) => {
    if (attempt < MAX_RETRIES && isTransientError(err)) {
      return new Promise<T>((resolve) =>
        setTimeout(
          () => resolve(invokeWithRetry<T>(cmd, args, attempt + 1)),
          RETRY_DELAYS[attempt],
        ),
      );
    }
    throw err;
  });
}

function isTransientError(err: unknown): boolean {
  const msg = err instanceof Error ? err.message : String(err);
  const transient = [
    "channel is closed",
    "channel busy",
    "failed to send",
    "connection closed",
    "resource unavailable",
  ];
  return transient.some((t) => msg.toLowerCase().includes(t));
}

export interface StreamInferenceRequest {
  prompt: string;
  maxTokens: number;
  temperature: number;
  topP: number;
  repeatPenalty?: number;
  stopWords: string[];
}

export interface AgentTaskRequest {
  prompt: string;
  maxTokens: number;
  temperature: number;
  topP: number;
  repeatPenalty?: number;
  maxSteps: number;
  stopWords: string[];
  planMode?: boolean;
  verify?: boolean;
  decompose?: boolean;
}

const onTokenEvent = "inference-token";
const onStartedEvent = "inference-started";
const onDoneEvent = "inference-done";
const onErrorEvent = "inference-error";
const onModelLoadedEvent = "model-loaded";
const onLoadProgressEvent = "model-load-progress";
const onToolEvent = "agent://tool-event";
const onAbortedEvent = "execution-aborted";
const onPermissionEvent = "agent://permission-request";
const onQuestionEvent = "agent://question-request";
const onKnowledgeEvent = "agent-knowledge";
const onFileChangedEvent = "agent://file-changed";
const onToolOutputEvent = "agent://tool-output";
const onStepEvent = "agent-step";
const onSubtaskEvent = "agent-subtask";
const onSkillsChangedEvent = "agent://skills-changed";
const onPlanStepEvent = "agent://plan-step";
const onTodoUpdateEvent = "agent://todo-update";
const onBgTaskEvent = "agent://bg-task-event";
const onWorkspaceChangedEvent = "workspace://file-changed";
export const api = {
  // ---- window chrome ----
  minimize: () => getCurrentWindow().minimize(),
  toggleMaximize: () => getCurrentWindow().toggleMaximize(),
  close: () => getCurrentWindow().close(),
  startDrag: () => getCurrentWindow().startDragging(),

  // ---- model ----
  pickAndLoadModel: (params: Partial<GenParams>) =>
    tauriInvoke<ModelInfo | null>("pick_and_load_model", {
      params: {
        nGpuLayers: params.nGpuLayers,
        contextSize: params.contextSize,
        nThreads: params.nThreads,
      },
    }),
  configureRemoteModel: (config: RemoteModelConfig) =>
    tauriInvoke<ModelInfo>("configure_remote_model", { config }),
  listRemoteModels: (config: {
    provider: string;
    baseUrl: string;
    apiKey: string;
  }) => tauriInvoke<string[]>("list_remote_models", { config }),

  // ---- multi-provider registry (routing) ----
  // Drives the backend `ProviderRegistry`. No UI is wired to these yet; the
  // commands exist so the frontend can register providers and route roles.
  providersUpsert: (provider: ProviderConfig) =>
    tauriInvoke<string>("providers_upsert", { provider }),
  providersRemove: (id: string) =>
    tauriInvoke<boolean>("providers_remove", { id }),
  providersSetRole: (role: ProviderRole, providerId: string) =>
    tauriInvoke<void>("providers_set_role", { role, providerId }),
  providersClearRole: (role: ProviderRole) =>
    tauriInvoke<void>("providers_clear_role", { role }),
  providersRoute: (role: ProviderRole) =>
    tauriInvoke<ProviderConfig | null>("providers_route", { role }),
  providersList: () =>
    tauriInvoke<ProviderConfig[]>("providers_list"),

  unloadModel: () => tauriInvoke<void>("unload_model"),
  modelStatus: () => tauriInvoke<ModelInfo | null>("model_status"),
  loadedModelPath: () => tauriInvoke<string | null>("loaded_model_path"),
  streamInference: (request: StreamInferenceRequest) =>
    tauriInvoke<number>("stream_inference", { request }),
  cancelInference: () => tauriInvoke<void>("cancel_inference"),
  agentRunTask: (request: AgentTaskRequest) =>
    tauriInvoke<number>("agent_run_task", { request }),

  // ---- background tasks (P2-12) ----
  agentRunBackground: (request: AgentTaskRequest) =>
    tauriInvoke<number>("agent_run_background", { request }),
  listBackgroundTasks: () =>
    tauriInvoke<BackgroundTaskInfo[]>("list_background_tasks"),
  abortBackgroundTask: (taskId: string) =>
    tauriInvoke<void>("abort_background_task", { taskId }),

  // ---- circuit breaker ----
  abortAgentExecution: () => tauriInvoke<{ message: string; sessionId: number }>("abort_agent_execution"),

  // ---- context eviction engine ----
  contextStatus: () => tauriInvoke<ContextUsage>("context_status"),
  contextPushTurn: (role: string, content: string) =>
    tauriInvoke<ContextUsage>("context_push_turn", { role, content }),
  contextSetSystemPrompt: (content: string) =>
    tauriInvoke<ContextUsage>("context_set_system_prompt", { content }),
  contextSetFileBuffer: (content: string) =>
    tauriInvoke<ContextUsage>("context_set_file_buffer", { content }),

  // ---- workspace / files ----
  pickWorkspaceFolder: () => tauriInvoke<string | null>("pick_workspace_folder"),
  pickTextFile: () => tauriInvoke<string | null>("pick_text_file"),
  agentSetWorkspace: (root: string) => tauriInvoke<void>("agent_set_workspace", { root }),
  agentGetWorkspaces: () => tauriInvoke<string[]>("agent_get_workspaces"),
  agentAddWorkspace: (root: string) => tauriInvoke<string[]>("agent_add_workspace", { root }),
  agentRemoveWorkspace: (root: string) => tauriInvoke<string[]>("agent_remove_workspace", { root }),
  listDirectory: (root: string, relative: string | null = null) =>
    tauriInvoke<FileNode[]>("list_directory", { root, relative }),
  readTextFile: (path: string) =>
    tauriInvoke<{ path: string; content: string }>("read_text_file", { path }),
  writeTextFile: (path: string, content: string) =>
    tauriInvoke<void>("write_text_file", { path, content }),
  revertFile: (path: string, before: string) =>
    tauriInvoke<void>("revert_file", { path, before }),
  saveFileAs: (content: string) =>
    tauriInvoke<string | null>("save_file_as", { content }),
  saveFileAsBytes: (content: string, suggestedFilename: string) =>
    tauriInvoke<string | null>("save_file_as_bytes", { content, suggestedFilename }),

  // ---- file watcher ----
  startFileWatcher: (path: string) =>
    tauriInvoke<void>("start_file_watcher", { path }),
  stopFileWatcher: () => tauriInvoke<void>("stop_file_watcher"),
  fileWatcherActive: () => tauriInvoke<boolean>("file_watcher_active"),

  // ---- permissions ----
  agentRespondPermission: (requestId: string, decision: string) =>
    tauriInvoke<void>("agent_respond_permission", { requestId, decision }),
  agentRespondQuestion: (requestId: string, answer: string) =>
    tauriInvoke<void>("agent_respond_question", { requestId, answer }),
  agentPolicySnapshot: () => tauriInvoke<PolicySnapshot>("agent_policy_snapshot"),
  agentSetYolo: (on: boolean) => tauriInvoke<void>("agent_set_yolo", { on }),
  agentGrantPath: (path: string, mode: "read" | "write") =>
    tauriInvoke<void>("agent_grant_path", { path, mode }),
  agentRevokePath: (path: string) =>
    tauriInvoke<void>("agent_revoke_path", { path }),

  // ---- audit trail ----
  agentAuditLog: (limit?: number) =>
    tauriInvoke<AuditEntry[]>("agent_audit_log", { limit }),

  // ---- git checkpoints (direct, from UI) ----
  gitCheckpoint: (message?: string) =>
    tauriInvoke<ToolResultInfo>("agent_git_checkpoint_cmd", { message }),
  gitCheckpoints: () =>
    tauriInvoke<CheckpointInfo[]>("agent_git_checkpoints_cmd"),
  gitRevert: (commit?: string) =>
    tauriInvoke<ToolResultInfo>("agent_git_revert_cmd", { commit }),

  // ---- skills & rules ----
  knowledgeScan: () => tauriInvoke<KnowledgeReport>("knowledge_scan"),
  knowledgeReport: () => tauriInvoke<KnowledgeReport>("knowledge_report_cmd"),
  skillSetActive: (name: string, active: boolean) =>
    tauriInvoke<KnowledgeReport>("skill_set_active", { name, active }),
  skillInstall: (source: string, global: boolean) =>
    tauriInvoke<KnowledgeReport>("skill_install", { source, global }),
  skillUninstall: (name: string) =>
    tauriInvoke<KnowledgeReport>("skill_uninstall", { name }),

  // ---- MCP server catalog ----
  mcpCatalogLoad: () =>
    tauriInvoke<McpServerConfig[]>("mcp_catalog_load"),
  mcpCatalogSave: (servers: McpServerConfig[]) =>
    tauriInvoke<void>("mcp_catalog_save", { servers }),

  // ---- Hugging Face hub ----
  hfSearch: (query: string, limit = 20) =>
    tauriInvoke<HfModel[]>("hf_search", { query, limit }),
  hfDownloadModel: (repoId: string, fileName: string) =>
    tauriInvoke<void>("hf_download_model", { repoId, fileName }),
  hfCancelDownload: (repoId: string, fileName: string) =>
    tauriInvoke<void>("hf_cancel_download", { repoId, fileName }),
  listDownloadedModels: () =>
    tauriInvoke<DownloadedModel[]>("list_downloaded_models"),
  loadModelFromPath: (path: string) =>
    tauriInvoke<ModelInfo>("load_model_from_path", { path }),
  autoLoadModel: () =>
    tauriInvoke<ModelInfo | null>("auto_load_model"),
  consoleHistory: () => tauriInvoke<string[]>("console_history"),

  // ---- local OpenAI-compatible API server ----
  apiServerStart: (port?: number) =>
    tauriInvoke<ApiServerStatus>("api_server_start", { port }),
  apiServerStop: () => tauriInvoke<ApiServerStatus>("api_server_stop"),
  apiServerStatus: () => tauriInvoke<ApiServerStatus>("api_server_status"),

  // ---- RAG attachments ----
  agentAttachFile: (path: string) =>
    tauriInvoke<AttachedFileInfo>("agent_attach_file", { path }),
  agentDetachFile: (path: string) =>
    tauriInvoke<void>("agent_detach_file", { path }),
  agentListAttachments: () =>
    tauriInvoke<AttachedFileInfo[]>("agent_list_attachments"),

  // ---- voice dictation ----
  voiceTranscribeData: (data: number[], ext?: string) =>
    tauriInvoke<string>("voice_transcribe_data", { data, ext }),

  // ---- settings / session persistence ----
  settingsLoad: () => tauriInvoke<Record<string, unknown>>("settings_load"),
  settingsSave: (settings: Record<string, unknown>) =>
    tauriInvoke<void>("settings_save", { settings }),
  sessionAppend: (
    project: string,
    record: Record<string, unknown>,
    chatId?: string | null,
  ) => tauriInvokeWrite<void>("session_append", { project, record, chatId }),
  sessionLoad: (project: string, chatId?: string | null) =>
    tauriInvoke<Record<string, unknown>[]>("session_load", { project, chatId }),
  sessionProjects: () =>
    tauriInvoke<SessionProjectInfo[]>("session_projects"),
  sessionDeleteChat: (project: string, chatId: string) =>
    tauriInvoke<void>("session_delete_chat", { project, chatId }),

  // ---- headless boot smoke (CI) ----
  smokeActive: () => tauriInvoke<boolean>("smoke_active"),
  smokeBootOk: () => tauriInvoke<void>("smoke_boot_ok"),

  // ---- engine events ----
  subscribeEngineEvents: async (handlers: EngineHandlers) => {
    const results = await Promise.allSettled([
      listen(onStartedEvent, (e) =>
        handlers.onStarted(parseEvent(e.payload)),
      ),
      listen(onTokenEvent, (e) => handlers.onToken(parseEvent(e.payload))),
      listen(onDoneEvent, (e) => handlers.onDone(parseEvent(e.payload))),
      listen(onErrorEvent, (e) => handlers.onError(parseEvent(e.payload))),
      listen(onModelLoadedEvent, (e) =>
        handlers.onModelLoaded(parseEvent(e.payload)),
      ),
      listen(onLoadProgressEvent, (e) =>
        handlers.onLoadProgress(parseEvent(e.payload)),
      ),
      ...(handlers.onTool
        ? [listen(onToolEvent, (e) => handlers.onTool!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onAborted
        ? [listen(onAbortedEvent, (e) => handlers.onAborted!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onPermission
        ? [listen(onPermissionEvent, (e) => handlers.onPermission!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onQuestion
        ? [listen(onQuestionEvent, (e) => handlers.onQuestion!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onKnowledge
        ? [listen(onKnowledgeEvent, (e) => handlers.onKnowledge!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onFileChanged
        ? [listen(onFileChangedEvent, (e) => handlers.onFileChanged!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onToolOutput
        ? [listen(onToolOutputEvent, (e) => handlers.onToolOutput!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onStep
        ? [listen(onStepEvent, (e) => handlers.onStep!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onSubtask
        ? [listen(onSubtaskEvent, (e) => handlers.onSubtask!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onSkillsChanged
        ? [listen(onSkillsChangedEvent, (e) => handlers.onSkillsChanged!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onPlanStep
        ? [listen(onPlanStepEvent, (e) => handlers.onPlanStep!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onTodoUpdate
        ? [listen(onTodoUpdateEvent, (e) => handlers.onTodoUpdate!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onBgTask
        ? [listen(onBgTaskEvent, (e) => handlers.onBgTask!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onWorkspaceChanged
        ? [listen(onWorkspaceChangedEvent, (e) => handlers.onWorkspaceChanged!(parseEvent(e.payload)))]
        : []),
      ...(handlers.onContextTrimmed
        ? [listen(EVT_CONTEXT_TRIMMED, (e) => handlers.onContextTrimmed!(parseEvent(e.payload)))]
        : []),
    ]);
    const unlisteners: (() => void)[] = [];
    const rejected = results.find((r) => r.status === "rejected") as
      | PromiseRejectedResult
      | undefined;
    for (const r of results) {
      if (r.status === "fulfilled") unlisteners.push(r.value);
    }
    if (rejected) {
      for (const un of unlisteners) un();
      throw rejected.reason;
    }
    return () => {
      for (const un of unlisteners) un();
    };
  },
};
