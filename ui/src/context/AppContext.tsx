import React, { createContext, useContext, useState, useEffect, useCallback, useMemo, useRef } from "react";
import { api, isTauriRuntime } from "../lib/ipc";
import {
  activeLocalConfig,
  mergeLocalConfig,
  normalizedConfig,
  sanitizeLocalConfig,
  type LocalModelConfig,
  LOCAL_CONFIG_KEYS,
} from "../lib/localModelConfig";
import { buildSystemPrompt } from "../lib/agentPrepare";
import { hydrateContextFromSession, recordToMessage } from "../lib/agentHistory";
import { splitConversationId } from "../lib/sessionLog";
import {
  defaultContextState,
  type ActiveFileBuffer,
  type ActivityLogEntry,
  type ChatMessage,
  type ContextState,
  type ConversationSummary,
  type LastModelSelection,
  type LlmSettings,
  type MessageUpdate,
  type ModelInfo,
} from "./types";

interface AppContextValue {
  theme: string;
  setTheme: (t: string) => void;
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
  historyPanelOpen: boolean;
  setHistoryPanelOpen: (v: boolean) => void;
  toggleHistoryPanel: () => void;
  modelName: string;
  modelProvider: string;
  setModelInfo: (name: string, provider: string) => void;
  modelMeta: ModelInfo | null;
  setModelMeta: (info: ModelInfo | null) => void;
  modelLoading: boolean;
  modelLoadProgress: number | null;
modelLoadStage: string;
  apiServerStatus: { running: boolean; port: number | null };
  unloadModel: (() => Promise<void>) | (() => void);
  projectPath: string;
  setProjectPath: (p: string) => void;
  backendConnected: boolean;
  setBackendConnected: (v: boolean) => void;
isProcessing: boolean;
  setProcessing: (v: boolean) => void;
  verify: boolean;
  setVerify: (v: boolean) => void;
  /** YOLO sub-mode: auto-approve ROUTINE shell commands (red-zone stays blocked). */
  yolo: boolean;
  setYolo: (v: boolean) => void;
  /** Max agent execution steps per task (1-20). */
  maxSteps: number;
  setMaxSteps: (v: number) => void;
  hitlGate: { requestId: string; reason: string } | null;
  setHitlGate: React.Dispatch<React.SetStateAction<{ requestId: string; reason: string } | null>>;
messages: ChatMessage[];
  addMessage: (msg: ChatMessage) => void;
  updateLastMessage: (update: MessageUpdate | ((prev: string) => string)) => void;
  clearMessages: () => void;
  truncateMessages: (fromIndex: number) => void;
  activityLog: ActivityLogEntry[];
  addActivity: (text: string, type: ActivityLogEntry["type"]) => void;
  clearActivity: () => void;
  composerDraft: string;
  setComposerDraft: (v: React.SetStateAction<string>) => void;
  currentMode: string;
  setMode: (m: string) => void;
  contextPaths: string[];
  addContextPath: (p: string) => void;
  removeContextPath: (p: string) => void;
  contextState: ContextState;
  setContextState: (partial: Partial<ContextState>) => void;
  clearContext: () => void;
  /** Active editor text, mirrored from EditorPane and pinned into agent context. */
  activeFile: ActiveFileBuffer | null;
  setActiveFile: (f: ActiveFileBuffer | null) => void;
  conversations: ConversationSummary[];
  setConversations: (c: ConversationSummary[]) => void;
  currentConversationId: string | null;
  setCurrentConversation: (id: string | null) => void;
  newChatSession: () => void;
  /** Load a stored chat: hydrate the message list and replay it into the
   *  backend ContextManager so the agent remembers the conversation. */
  loadConversation: (conversationId: string) => Promise<void>;
  loadingConversation: boolean;
  chatAbortRef: React.MutableRefObject<AbortController | null>;
  settings: Record<string, unknown>;
  setSettings: (s: Record<string, unknown>) => void;
llmSettings: LlmSettings;
  loadLlmSettings: () => Promise<void>;
  saveLlmSettings: (patch: Partial<LlmSettings>) => void | Promise<void>;
  modelLoadParams: () => Record<string, unknown>;
  /** Per-model Local Model configs keyed by GGUF path (`"default"` = global). */
  localModelConfigs: Record<string, Partial<LocalModelConfig>>;
  loadLocalModelConfigs: () => Promise<void>;
  saveLocalModelConfig: (
    path: string | null,
    patch: Partial<LocalModelConfig>,
  ) => void | Promise<void>;
  /** Path of the currently loaded local model (null = none/remote). */
  activeLocalModelPath: string | null;
  /** Merged config currently in effect for the active local model. */
  activeModelConfig: Partial<LocalModelConfig> | undefined;
rememberLastModel: (sel: LastModelSelection) => void;
  lastModel: LastModelSelection | null;
  /** Startup restore of the last model: probe availability/connectivity before showing it as loaded. */
  modelRestoreState: "idle" | "loading" | "ready" | "failed";
  modelRestoreError: string;
  modelPromptVisible: boolean;
  dismissModelPrompt: () => void;
  /** Open the "pick a model" prompt (used on send when no model is loaded). */
  promptModelSelection: () => void;
}

export type {
  ActiveFileBuffer,
  ActivityLogEntry,
  ChatMessage,
  ContextState,
  ConversationSummary,
  LastModelSelection,
  LlmSettings,
  MessageUpdate,
  ModelInfo,
};

export const LAST_MODEL_KEY = "ai-editor.lastModel";

const AppContext = createContext<AppContextValue | null>(null);

export function useApp() {
  const ctx = useContext(AppContext);
  if (!ctx) throw new Error("useApp must be used within AppProvider");
  return ctx;
}

const DARK_THEMES = new Set(["dark", "midnight", "ocean", "forest"]);

function applyTheme(theme: string) {
  const root = document.documentElement;
  root.classList.remove("theme-dark", "theme-contrast");
  let effective = theme;
  if (theme === "system") {
    effective = window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  root.setAttribute("data-theme", effective);
  if (effective === "high-contrast") {
    root.classList.add("theme-contrast");
  } else if (DARK_THEMES.has(effective)) {
    root.classList.add("theme-dark");
  }
}

export function AppProvider({ children }: { children: React.ReactNode }) {
  const [theme, setThemeState] = useState(() => localStorage.getItem("theme") || "light");
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => localStorage.getItem("sidebarCollapsed") === "true");
  const [historyPanelOpen, setHistoryPanelOpen] = useState(false);
  const [modelName, setModelName] = useState("");
  const [modelProvider, setModelProvider] = useState("Local");
  const [modelMeta, setModelMeta] = useState<ModelInfo | null>(null);
const [modelLoading, setModelLoading] = useState(false);
  const [modelLoadProgress, setModelLoadProgress] = useState<number | null>(null);
  const [modelLoadStage, setModelLoadStage] = useState("");
  const [apiServerStatus, setApiServerStatus] = useState<{ running: boolean; port: number | null }>({
    running: false,
    port: null,
  });
  const [projectPath, setProjectPathState] = useState(() => localStorage.getItem("projectPath") || "");
  const [backendConnected, setBackendConnected] = useState(false);
const [isProcessing, setIsProcessing] = useState(false);
  const [verify, setVerifyState] = useState(true);
  const [yolo, setYoloState] = useState(() => localStorage.getItem("ai-editor.yolo") === "true");
  const [maxSteps, setMaxStepsState] = useState(() => {
    const raw = Number(localStorage.getItem("ai-editor.maxSteps"));
    return Number.isFinite(raw) && raw >= 1 && raw <= 20 ? raw : 12;
  });
  const [hitlGate, setHitlGateState] = useState<{ requestId: string; reason: string } | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [activityLog, setActivityLog] = useState<ActivityLogEntry[]>([]);
  const [composerDraft, setComposerDraft] = useState("");
  const [currentMode, setCurrentMode] = useState("agent");
  const [contextPaths, setContextPaths] = useState<string[]>([]);
  const [contextState, setContextStateRaw] = useState<ContextState>(defaultContextState);
  const [activeFile, setActiveFile] = useState<ActiveFileBuffer | null>(null);
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [currentConversationId, setCurrentConversation] = useState<string | null>(null);
  const [settings, setSettings] = useState<Record<string, unknown>>({});
  const [llmSettings, setLlmSettings] = useState<LlmSettings>({});
  const [localModelConfigs, setLocalModelConfigs] = useState<Record<string, Partial<LocalModelConfig>>>({});
  const [lastModel, setLastModel] = useState<LastModelSelection | null>(() => {
    try {
      const raw = localStorage.getItem(LAST_MODEL_KEY);
      return raw ? (JSON.parse(raw) as LastModelSelection) : null;
    } catch {
      return null;
    }
  });
const chatAbortRef = useRef<AbortController | null>(null);
  const restoreStartedRef = useRef(false);
  const activitySeqRef = useRef(1);
  const [modelRestoreState, setModelRestoreState] = useState<"idle" | "loading" | "ready" | "failed">("idle");
  const [modelRestoreError, setModelRestoreError] = useState("");
  const [modelPromptVisible, setModelPromptVisible] = useState(false);

  const rememberLastModel = useCallback((sel: LastModelSelection) => {
    setLastModel(sel);
    setModelRestoreState("ready");
    setModelRestoreError("");
    setModelPromptVisible(false);
    try {
      localStorage.setItem(LAST_MODEL_KEY, JSON.stringify(sel));
    } catch {
      // storage unavailable — skip persistence
    }
  }, []);

  const dismissModelPrompt = useCallback(() => {
    setModelPromptVisible(false);
  }, []);

  const promptModelSelection = useCallback(() => {
    setModelPromptVisible(true);
  }, []);

useEffect(() => { applyTheme(theme); localStorage.setItem("theme", theme); }, [theme]);
  useEffect(() => { localStorage.setItem("sidebarCollapsed", String(sidebarCollapsed)); }, [sidebarCollapsed]);

  const refreshServerStatus = useCallback(() => {
    api.apiServerStatus().then(setApiServerStatus).catch(() => {});
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    refreshServerStatus();
  }, [refreshServerStatus]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    const refresh = refreshServerStatus;
    api
      .subscribeEngineEvents({
        onLoadProgress: (data) => {
          const stage = (data as { stage?: string }).stage;
          const progress = (data as { progress?: unknown }).progress;
          if (stage === "done") {
            setModelLoading(false);
            setModelLoadProgress(null);
            setModelLoadStage("");
          } else if (stage === "error") {
            setModelLoading(false);
            setModelLoadProgress(null);
            setModelLoadStage("error");
          } else {
            setModelLoading(true);
            setModelLoadStage(stage || "load");
            if (typeof progress === "number") setModelLoadProgress(progress);
          }
        },
        onModelLoaded: (data) => {
          setModelLoading(false);
          setModelLoadProgress(null);
          setModelLoadStage("");
          refresh();
          if (data && typeof data === "object" && "nCtxTrain" in data) {
            setModelMeta(data as unknown as ModelInfo);
          }
        },
      })
      .then((un) => {
        if (disposed) un();
        else unlisten = un;
      })
      .catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [refreshServerStatus]);

  const setTheme = useCallback((t: string) => setThemeState(t), []);
  const toggleSidebar = useCallback(() => setSidebarCollapsed((p) => !p), []);
  const toggleHistoryPanel = useCallback(() => setHistoryPanelOpen((p) => !p), []);

  const setModelInfo = useCallback((name: string, provider: string) => {
    setModelName(name);
    setModelProvider(provider);
    setModelLoading(false);
    setModelLoadProgress(null);
    setModelLoadStage("");
  }, []);

  const setModelMetaSafe = useCallback((info: ModelInfo | null) => {
    setModelMeta(info);
  }, []);

const unloadModel = useCallback(async () => {
    try {
      await api.unloadModel();
    } catch {
      // best-effort; local models still unloaded by the engine
    }
    refreshServerStatus();
setModelMeta(null);
    setModelInfo("", "Local");
    setModelRestoreState("idle");
    setModelRestoreError("");
    setModelPromptVisible(false);
  }, [setModelInfo, refreshServerStatus]);

  const setProjectPath = useCallback((p: string) => {
    setProjectPathState(p);
    localStorage.setItem("projectPath", p);
  }, []);

  const addMessage = useCallback((msg: ChatMessage) => {
    setMessages((prev) => [...prev, { ...msg, ts: msg.ts || Date.now() }]);
  }, []);

  const updateLastMessage = useCallback((update: MessageUpdate | ((prev: string) => string)) => {
    setMessages((prev) => {
      if (prev.length === 0) return prev;
      const last = prev[prev.length - 1];
      let updated: ChatMessage;
      if (typeof update === "function") {
        updated = { ...last, content: update(last.content) };
      } else {
        const content =
          typeof update.content === "function"
            ? update.content(last.content)
            : update.content ?? last.content;
        const { content: _drop, ...rest } = update;
        updated = { ...last, ...rest, content };
      }
      return [...prev.slice(0, -1), updated];
    });
  }, []);

  const clearMessages = useCallback(() => setMessages([]), []);

  const truncateMessages = useCallback((fromIndex: number) => {
    setMessages((prev) => (fromIndex >= 0 ? prev.slice(0, fromIndex) : prev));
  }, []);

  const addActivity = useCallback((text: string, type: ActivityLogEntry["type"]) => {
    setActivityLog((prev) => [...prev.slice(-50), { id: activitySeqRef.current++, text, type }]);
  }, []);

  const clearActivity = useCallback(() => setActivityLog([]), []);

  const setMode = useCallback((m: string) => setCurrentMode(m), []);

  const setVerify = useCallback((v: boolean) => setVerifyState(v), []);

  const setYolo = useCallback((v: boolean) => {
    setYoloState(v);
    try {
      localStorage.setItem("ai-editor.yolo", String(v));
    } catch {
      // storage unavailable — skip persistence
    }
  }, []);

  const setMaxSteps = useCallback((v: number) => {
    const clamped = Math.min(20, Math.max(1, Math.round(v)));
    setMaxStepsState(clamped);
    try {
      localStorage.setItem("ai-editor.maxSteps", String(clamped));
    } catch {
      // storage unavailable — skip persistence
    }
  }, []);

  const setHitlGate = useCallback(
    (gate: React.SetStateAction<{ requestId: string; reason: string } | null>) => {
      setHitlGateState(gate);
    },
    [],
  );

  const addContextPath = useCallback((p: string) => {
    setContextPaths((prev) => prev.includes(p) ? prev : [...prev, p]);
  }, []);

  const removeContextPath = useCallback((p: string) => {
    setContextPaths((prev) => prev.filter((x) => x !== p));
  }, []);

  const setContextState = useCallback((partial: Partial<ContextState>) => {
    setContextStateRaw((prev) => ({ ...prev, ...partial }));
  }, []);

  const clearContext = useCallback(() => {
    setContextPaths([]);
    setContextStateRaw(defaultContextState);
  }, []);

  const newChatSession = useCallback(() => {
    setMessages([]);
    setActivityLog([]);
    setCurrentConversation(null);
    setComposerDraft("");
    setActiveFile(null);
    clearContext();
  }, [clearContext]);

  const [loadingConversation, setLoadingConversation] = useState(false);

  const loadConversation = useCallback(
    async (conversationId: string) => {
      const { project, chatId } = splitConversationId(conversationId);
      setLoadingConversation(true);
      try {
        const records = await api.sessionLoad(project, chatId);
        const messages = records
          .map((r) => recordToMessage(r))
          .filter((m): m is NonNullable<typeof m> => m !== null)
          .map((m) => ({ ...m, streaming: false }));
        setMessages(messages);
        setActivityLog([]);
        setCurrentConversation(conversationId);
        // Replay into the backend so the next turn has the prior context.
        await hydrateContextFromSession(
          records,
          buildSystemPrompt(currentMode, modelMeta?.nParams),
        );
      } catch (err) {
        addActivity(
          `Could not load that chat: ${err instanceof Error ? err.message : String(err)}`,
          "error",
        );
      } finally {
        setLoadingConversation(false);
      }
    },
    [currentMode, modelMeta?.nParams, addActivity],
  );

  const loadLlmSettings = useCallback(async () => {    try {
      const s = await api.settingsLoad();
      const params = (s && (s as Record<string, unknown>).params) as Record<string, unknown> | undefined;
      const nWorkers = s && (s as Record<string, unknown>).nWorkers;
      const merged: Record<string, unknown> = { ...((params || {}) as Record<string, unknown>) };
      if (typeof nWorkers === "number") merged.nWorkers = nWorkers;
      setLlmSettings(merged as LlmSettings);
    } catch { /* ignore */ }
  }, []);

  const llmDirtyRef = useRef<LlmSettings | null>(null);
  const llmSaveQueue = useRef<Promise<void>>(Promise.resolve());

  const flushLlmSettings = useCallback(() => {
    const next = llmDirtyRef.current;
    if (!next) return Promise.resolve();
    llmDirtyRef.current = null;
    return (async () => {
      try {
        const current = (await api.settingsLoad()) || {};
        const saved: Record<string, unknown> = { ...current };
        const { nWorkers, ...params } = next;
        saved.params = params;
        if (nWorkers !== undefined) saved.nWorkers = nWorkers;
        await api.settingsSave(saved);
      } catch { /* ignore */ }
    })();
  }, []);

const saveLlmSettings = useCallback((patch: Partial<LlmSettings>) => {
    setLlmSettings((prev) => {
      const next = { ...prev, ...patch };
      llmDirtyRef.current = next;
      return next;
    });
    llmSaveQueue.current = llmSaveQueue.current.then(flushLlmSettings);
  }, [flushLlmSettings]);

  const loadLocalModelConfigs = useCallback(async () => {
    try {
      const configs = await api.localModelConfigsLoad();
      const clean: Record<string, Partial<LocalModelConfig>> = {};
      for (const [k, v] of Object.entries(configs ?? {})) {
        clean[k] = sanitizeLocalConfig((v ?? {}) as Record<string, unknown>);
      }
      setLocalModelConfigs(clean);
    } catch { /* ignore */ }
  }, []);

  const saveLocalModelConfig = useCallback(
    (path: string | null, patch: Partial<LocalModelConfig>) => {
      setLocalModelConfigs((prev) => {
        const key = path || "default";
        const cleanPatch = normalizedConfig(patch);
        const next = { ...prev, [key]: mergeLocalConfig(prev[key], cleanPatch) };
        void api.saveLocalModelConfig(key === "default" ? null : key, next[key] || {}).catch(() => {});
        return next;
      });
    },
    [],
  );

  const activeLocalModelPath = lastModel?.kind === "local" ? lastModel.path : null;

  const activeModelConfig = useMemo(
    () => activeLocalConfig(localModelConfigs, activeLocalModelPath),
    [localModelConfigs, activeLocalModelPath],
  );

  const modelLoadParams = useCallback((): Record<string, unknown> => {
    const out: Record<string, unknown> = {};
    if (llmSettings.contextSize != null) out.contextSize = llmSettings.contextSize;
    if (llmSettings.nThreads != null) out.nThreads = llmSettings.nThreads;
    if (llmSettings.nGpuLayers != null) out.nGpuLayers = llmSettings.nGpuLayers;
    if (llmSettings.nWorkers != null) out.nWorkers = llmSettings.nWorkers;
    // Per-model config from the Local Model page wins where it is defined, so
    // the UI edits genuinely reach load_model_params on the next load.
    const active = activeLocalConfig(localModelConfigs, activeLocalModelPath);
    if (active) {
      for (const key of LOCAL_CONFIG_KEYS) {
        const v = active[key];
        if (v !== undefined) out[key] = v;
      }
    }
    return out;
  }, [llmSettings, localModelConfigs, activeLocalModelPath]);

  useEffect(() => { loadLlmSettings(); }, [loadLlmSettings]);
  useEffect(() => { loadLocalModelConfigs(); }, [loadLocalModelConfigs]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .agentSetYolo(yolo)
      .catch(() => {});
  }, [yolo]);

  useEffect(() => {
    if (!isTauriRuntime() || !projectPath) return;
    api.agentSetWorkspace(projectPath).catch(() => {});
    api.contextSetSystemPrompt(buildSystemPrompt(currentMode, modelMeta?.nParams)).catch(() => {});
  }, [projectPath, currentMode, modelMeta?.nParams]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .agentPolicySnapshot()
      .then((snap) => {
        const y = (snap as { yolo?: unknown }).yolo;
        if (typeof y === "boolean") setYoloState(y);
      })
      .catch(() => {});
  }, [projectPath]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .modelStatus()
      .then((info) => {
        if (info && typeof info === "object" && "nCtxTrain" in info) {
          setModelMeta(info as unknown as ModelInfo);
        }
      })
      .catch(() => {});
  }, []);

useEffect(() => {
    if (!isTauriRuntime()) return;
    if (restoreStartedRef.current) return;
    restoreStartedRef.current = true;
    if (!lastModel) return;
    setModelRestoreState("loading");
    setModelLoading(true);
    setModelLoadStage("reconnect");
    (async () => {
      try {
        // If the backend already has a model loaded (e.g. same-session reload),
        // adopt it instead of paying for a second load.
        const existing = await api.modelStatus();
        if (existing) {
          if ("nCtxTrain" in existing) setModelMeta(existing as unknown as ModelInfo);
          setModelName(lastModel.name);
          setModelProvider(lastModel.kind === "remote" ? lastModel.providerLabel : "Local");
          setModelRestoreState("ready");
          return;
        }
        if (lastModel.kind === "local") {
          // Cheap existence check first — fail fast without a heavy load attempt.
          if (!(await api.pathExists(lastModel.path))) {
            throw new Error(
              `The saved model file is missing:\n${lastModel.path}\n\nDownload it again or pick a different model.`,
            );
          }
          const info = (await api.loadModelFromPath(lastModel.path, modelLoadParams())) as
            | { name?: string }
            | Record<string, unknown>
            | undefined;
          setModelName(String((info as { name?: string })?.name || lastModel.name));
          setModelProvider("Local");
        } else {
          if (!lastModel.baseUrl) throw new Error("No provider URL was saved for the last model.");
          if (!lastModel.apiKey) throw new Error("No API key was saved for the last model.");
          // Probe the provider (base URL + API key) before loading: only
          // reconnect the last remote model if the connection is actually live.
          await api.listRemoteModels({
            provider: lastModel.provider,
            baseUrl: lastModel.baseUrl,
            apiKey: lastModel.apiKey,
          });
          await api.configureRemoteModel({
            provider: lastModel.provider,
            baseUrl: lastModel.baseUrl,
            apiKey: lastModel.apiKey,
            model: lastModel.name,
          });
          setModelName(lastModel.name);
          setModelProvider(lastModel.providerLabel);
        }
        const fresh = await api.modelStatus().catch(() => null);
        if (fresh && "nCtxTrain" in fresh) setModelMeta(fresh as unknown as ModelInfo);
        setModelRestoreState("ready");
      } catch (err) {
        setModelName("");
        setModelProvider("Local");
        setModelMeta(null);
        setModelRestoreState("failed");
        setModelRestoreError(err instanceof Error ? err.message : String(err));
        setModelPromptVisible(true);
        addActivity("Could not reconnect to the last model — pick one in the Models page.", "error");
      } finally {
        setModelLoading(false);
        setModelLoadStage("");
      }
    })();
  }, [lastModel, modelLoadParams, addActivity]);

  const value: AppContextValue = {
    theme, setTheme,
    sidebarCollapsed, toggleSidebar,
    historyPanelOpen, setHistoryPanelOpen, toggleHistoryPanel,
    modelName, modelProvider, setModelInfo,
    modelMeta, setModelMeta: setModelMetaSafe,
    modelLoading, modelLoadProgress, modelLoadStage, unloadModel,
    apiServerStatus,
    projectPath, setProjectPath,
    backendConnected, setBackendConnected,
isProcessing, setProcessing: setIsProcessing,
    verify, setVerify,
    yolo, setYolo,
    maxSteps, setMaxSteps,
    hitlGate, setHitlGate,
    messages, addMessage, updateLastMessage, clearMessages, truncateMessages,
    activityLog, addActivity, clearActivity,
    composerDraft, setComposerDraft,
    currentMode, setMode,
    contextPaths, addContextPath, removeContextPath,
    contextState, setContextState, clearContext,
    activeFile, setActiveFile,
    conversations, setConversations,
    currentConversationId, setCurrentConversation,
    newChatSession,
    loadConversation,
    loadingConversation,
    chatAbortRef,
    settings, setSettings,
llmSettings, loadLlmSettings, saveLlmSettings, modelLoadParams,
    localModelConfigs, loadLocalModelConfigs, saveLocalModelConfig,
    activeLocalModelPath, activeModelConfig,
rememberLastModel, lastModel,
    modelRestoreState, modelRestoreError, modelPromptVisible, dismissModelPrompt,
    promptModelSelection,
  };

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

