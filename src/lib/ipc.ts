import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

import type {
  ContextUsage,
  GenParams,
  FileNode,
  KnowledgeReport,
  ModelInfo,
  PolicySnapshot,
  RemoteModelConfig,
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
  agentRespondPermission: (requestId: string, allowed: boolean) =>
    tauriInvoke<void>("agent_respond_permission", { requestId, allowed }),
  agentPolicySnapshot: () => tauriInvoke<PolicySnapshot>("agent_policy_snapshot"),

  // ---- skills & rules ----
  knowledgeScan: () => tauriInvoke<KnowledgeReport>("knowledge_scan"),
  knowledgeReport: () => tauriInvoke<KnowledgeReport>("knowledge_report_cmd"),
  skillSetActive: (name: string, active: boolean) =>
    tauriInvoke<KnowledgeReport>("skill_set_active", { name, active }),

  // ---- settings / session persistence ----
  settingsLoad: () => tauriInvoke<Record<string, unknown>>("settings_load"),
  settingsSave: (settings: Record<string, unknown>) =>
    tauriInvoke<void>("settings_save", { settings }),
  sessionAppend: (project: string, record: Record<string, unknown>) =>
    tauriInvoke<void>("session_append", { project, record }),
  sessionLoad: (project: string) =>
    tauriInvoke<Record<string, unknown>[]>("session_load", { project }),

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
    ]);
    return () => {
      for (const un of unlisteners) un();
    };
  },
};
