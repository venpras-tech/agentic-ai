import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type {
  ApiServerStatus,
  AttachedFileInfo,
  AuditEntry,
  ContextUsage,
  DownloadedModel,
  GenParams,
  FileNode,
  HfModel,
  KnowledgeReport,
  McpServerConfig,
  ModelInfo,
  PolicySnapshot,
  RemoteModelConfig,
  SessionProjectInfo,
  CheckpointInfo,
  ToolResultInfo,
} from "../types";
import { parseEvent, type EngineHandlers } from "./events";

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
  return invoke<T>(cmd, args);
}

export interface StreamInferenceRequest {
  prompt: string;
  maxTokens: number;
  temperature: number;
  topP: number;
  stopWords: string[];
}

export interface AgentTaskRequest {
  prompt: string;
  maxTokens: number;
  temperature: number;
  topP: number;
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
const onKnowledgeEvent = "agent-knowledge";
const onFileChangedEvent = "agent://file-changed";
const onToolOutputEvent = "agent://tool-output";
const onStepEvent = "agent-step";
const onSubtaskEvent = "agent-subtask";
const onSkillsChangedEvent = "agent://skills-changed";
const onPlanStepEvent = "agent://plan-step";
const onTodoUpdateEvent = "agent://todo-update";

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
  unloadModel: () => tauriInvoke<void>("unload_model"),
  modelStatus: () => tauriInvoke<ModelInfo | null>("model_status"),
  loadedModelPath: () => tauriInvoke<string | null>("loaded_model_path"),
  streamInference: (request: StreamInferenceRequest) =>
    tauriInvoke<number>("stream_inference", { request }),
  cancelInference: () => tauriInvoke<void>("cancel_inference"),
  agentRunTask: (request: AgentTaskRequest) =>
    tauriInvoke<number>("agent_run_task", { request }),

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
  listDirectory: (root: string, relative: string | null = null) =>
    tauriInvoke<FileNode[]>("list_directory", { root, relative }),
  readTextFile: (path: string) =>
    tauriInvoke<{ path: string; content: string }>("read_text_file", { path }),
  writeTextFile: (path: string, content: string) =>
    tauriInvoke<void>("write_text_file", { path, content }),
  saveFileAs: (content: string) =>
    tauriInvoke<string | null>("save_file_as", { content }),

  // ---- permissions ----
  agentRespondPermission: (requestId: string, decision: string) =>
    tauriInvoke<void>("agent_respond_permission", { requestId, decision }),
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
  ) => tauriInvoke<void>("session_append", { project, record, chatId }),
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
    const unlisteners = await Promise.all([
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
    ]);
    return () => {
      for (const un of unlisteners) un();
    };
  },
};
