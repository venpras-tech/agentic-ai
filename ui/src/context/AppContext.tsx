import React, { createContext, useContext, useState, useEffect, useCallback, useMemo, useRef } from "react";
import { api, isTauriRuntime } from "../lib/ipc";
import type { IndexStatus } from "../lib/ipc";
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
import { chatIdForInvoke, newChatId, splitConversationId } from "../lib/sessionLog";
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

/**
 * The app state is split into independent domain contexts (shell, model, agent
 * settings, chat, context-pane) so a change in one domain — a keystroke in the
 * composer, a model-load poll tick, a new activity line — only re-renders the
 * components that actually consume that domain. `useApp()` remains as a merged
 * convenience hook that subscribes to every domain.
 */
interface ShellValue {
  theme: string;
  setTheme: (t: string) => void;
  sidebarCollapsed: boolean;
  toggleSidebar: () => void;
  historyPanelOpen: boolean;
  setHistoryPanelOpen: (v: boolean) => void;
  toggleHistoryPanel: () => void;
  projectPath: string;
  setProjectPath: (p: string) => void;
  /** Current workspace text-index snapshot (updated on rebuild + progress events). */
  indexStatus: IndexStatus | null;
  /** True while a workspace index rebuild is in flight. */
  indexing: boolean;
  /** Manually rebuild the workspace index. */
  reindex: () => void;
  /** Recently opened workspace roots (most-recent-first, capped). */
  recentProjects: string[];
  backendConnected: boolean;
  setBackendConnected: (v: boolean) => void;
  hitlGate: { requestId: string; reason: string } | null;
  setHitlGate: React.Dispatch<React.SetStateAction<{ requestId: string; reason: string } | null>>;
}

interface ModelValue {
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
  llmSettings: LlmSettings;
  loadLlmSettings: () => Promise<void>;
  saveLlmSettings: (patch: Partial<LlmSettings>) => void | Promise<void>;
  modelLoadParams: () => Record<string, unknown>;
  localModelConfigs: Record<string, Partial<LocalModelConfig>>;
  loadLocalModelConfigs: () => Promise<void>;
  saveLocalModelConfig: (
    path: string | null,
    patch: Partial<LocalModelConfig>,
  ) => void | Promise<void>;
  activeLocalModelPath: string | null;
  activeModelConfig: Partial<LocalModelConfig> | undefined;
  rememberLastModel: (sel: LastModelSelection) => void;
  lastModel: LastModelSelection | null;
  modelRestoreState: "idle" | "loading" | "ready" | "failed";
  modelRestoreError: string;
  modelPromptVisible: boolean;
  dismissModelPrompt: () => void;
  promptModelSelection: () => void;
}

interface AgentSettingsValue {
  verify: boolean;
  setVerify: (v: boolean) => void;
  /** YOLO sub-mode: auto-approve ROUTINE shell commands (red-zone stays blocked). */
  yolo: boolean;
  setYolo: (v: boolean) => void;
  /** Max agent execution steps per task (1-20). */
  maxSteps: number;
  setMaxSteps: (v: number) => void;
  /** Total-token cap per agent run (0 = no cap). */
  tokenBudget: number;
  setTokenBudget: (v: number) => void;
  /** Run the goal-search phase before the focused execution. */
  decompose: boolean;
  setDecompose: (v: boolean) => void;
  /** Ask mode: single chat-completion turn (zero tool loop) instead of the
   *  agent orchestrator. Faster; no tools run. */
  preferChatComplete: boolean;
  setPreferChatComplete: (v: boolean) => void;
  settings: Record<string, unknown>;
  setSettings: (s: Record<string, unknown>) => void;
}

interface ChatValue {
  messages: ChatMessage[];
  addMessage: (msg: ChatMessage) => void;
  updateLastMessage: (update: MessageUpdate | ((prev: string) => string)) => void;
  clearMessages: () => void;
  truncateMessages: (fromIndex: number) => void;
  isProcessing: boolean;
  setProcessing: (v: boolean) => void;
  composerDraft: string;
  setComposerDraft: (v: React.SetStateAction<string>) => void;
  currentMode: string;
  setMode: (m: string) => void;
  activityLog: ActivityLogEntry[];
  addActivity: (text: string, type: ActivityLogEntry["type"]) => void;
  clearActivity: () => void;
  conversations: ConversationSummary[];
  setConversations: (c: ConversationSummary[]) => void;
  /** Increments whenever a turn is persisted, so the history list can refetch
   *  without the panel having to be closed and reopened. */
  historyVersion: number;
  bumpHistory: () => void;
  currentConversationId: string | null;
  setCurrentConversation: (id: string | null) => void;
  newChatSession: () => void;
  /** Load a stored chat: hydrate the message list and replay it into the
   *  backend ContextManager so the agent remembers the conversation. */
  loadConversation: (conversationId: string) => Promise<void>;
  loadingConversation: boolean;
}

interface ContextPaneValue {
  contextPaths: string[];
  addContextPath: (p: string) => void;
  removeContextPath: (p: string) => void;
  contextState: ContextState;
  setContextState: (partial: Partial<ContextState>) => void;
  clearContext: () => void;
  /** Active editor text, mirrored from EditorPane and pinned into agent context. */
  activeFile: ActiveFileBuffer | null;
  setActiveFile: (f: ActiveFileBuffer | null) => void;
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

/// Older builds wrote the provider API key into `LAST_MODEL_KEY` as plaintext.
/// Read it once so an upgrade can hand it to the backend (which persists it in
/// `active-remote.json`), then the caller scrubs it from localStorage.
function legacyApiKeyFromStorage(): string {
  try {
    const raw = localStorage.getItem(LAST_MODEL_KEY);
    if (!raw) return "";
    const parsed = JSON.parse(raw) as { apiKey?: unknown };
    return typeof parsed?.apiKey === "string" ? parsed.apiKey : "";
  } catch {
    return "";
  }
}

function scrubLegacyApiKey(): void {
  try {
    const raw = localStorage.getItem(LAST_MODEL_KEY);
    if (!raw) return;
    const parsed = JSON.parse(raw) as Record<string, unknown> | null;
    if (parsed && typeof parsed === "object" && "apiKey" in parsed) {
      delete parsed.apiKey;
      localStorage.setItem(LAST_MODEL_KEY, JSON.stringify(parsed));
    }
  } catch {
    // storage unavailable — nothing to scrub
  }
}

const ShellContext = createContext<ShellValue | null>(null);
const ModelContext = createContext<ModelValue | null>(null);
const AgentSettingsContext = createContext<AgentSettingsValue | null>(null);
const ChatContext = createContext<ChatValue | null>(null);
const ContextPaneContext = createContext<ContextPaneValue | null>(null);

function useDomain<T>(ctx: React.Context<T | null>, name: string): T {
  const value = useContext(ctx);
  if (!value) throw new Error(`${name} must be used within AppProvider`);
  return value;
}

/** Shell/chrome state: theme, sidebar, panels, workspace path, backend link. */
export function useShell() {
  return useDomain(ShellContext, "useShell");
}

/** Model + LLM state: loaded model, load progress, settings, local configs. */
export function useModel() {
  return useDomain(ModelContext, "useModel");
}

/** Agent tunables: verify, YOLO, step/token budgets, decompose, raw settings. */
export function useAgentSettings() {
  return useDomain(AgentSettingsContext, "useAgentSettings");
}

/** Conversation state: messages, streaming flag, drafts, history, activity log. */
export function useChat() {
  return useDomain(ChatContext, "useChat");
}

/** Pinned context: context paths, context-window state, active editor buffer. */
export function useContextPane() {
  return useDomain(ContextPaneContext, "useContextPane");
}

/**
 * Merged view of every domain. Prefer the focused hooks above so a component
 * only re-renders for the state it actually reads; this hook re-renders on any
 * domain change and exists for components that genuinely span all of them.
 */
export function useApp() {
  return {
    ...useShell(),
    ...useModel(),
    ...useAgentSettings(),
    ...useChat(),
    ...useContextPane(),
  };
}

const DARK_THEMES = new Set(["dark", "midnight", "ocean", "forest"]);

const RECENT_PROJECTS_KEY = "ai-editor.recentProjects";

function readRecentProjects(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_PROJECTS_KEY);
    const arr = raw ? JSON.parse(raw) : [];
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function recordRecentProject(path: string, prev: string[]): string[] {
  if (!path) return prev;
  const next = [path, ...prev.filter((p) => p !== path)].slice(0, 8);
  try {
    localStorage.setItem(RECENT_PROJECTS_KEY, JSON.stringify(next));
  } catch {
    void 0;
  }
  return next;
}


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
  const [recentProjects, setRecentProjects] = useState<string[]>(readRecentProjects);
  const [indexStatus, setIndexStatus] = useState<IndexStatus | null>(null);
  const [indexing, setIndexing] = useState(false);
  const [backendConnected, setBackendConnected] = useState(false);
const [isProcessing, setIsProcessing] = useState(false);
  const [verify, setVerifyState] = useState(true);
  const [yolo, setYoloState] = useState(() => localStorage.getItem("ai-editor.yolo") === "true");
  const [maxSteps, setMaxStepsState] = useState(() => {
    const raw = Number(localStorage.getItem("ai-editor.maxSteps"));
    return Number.isFinite(raw) && raw >= 1 && raw <= 20 ? raw : 12;
  });
  const [tokenBudget, setTokenBudgetState] = useState(() => {
    const raw = Number(localStorage.getItem("ai-editor.tokenBudget"));
    return Number.isFinite(raw) && raw >= 0 ? raw : 0;
  });
  const [decompose, setDecomposeState] = useState(false);
  const [preferChatComplete, setPreferChatCompleteState] = useState(
    () => localStorage.getItem("ai-editor.preferChatComplete") === "true",
  );
  const [hitlGate, setHitlGateState] = useState<{ requestId: string; reason: string } | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [activityLog, setActivityLog] = useState<ActivityLogEntry[]>([]);
  const [composerDraft, setComposerDraft] = useState("");
  const [currentMode, setCurrentMode] = useState("agent");
  const [contextPaths, setContextPaths] = useState<string[]>([]);
  const [contextState, setContextStateRaw] = useState<ContextState>(defaultContextState);
  const [activeFile, setActiveFile] = useState<ActiveFileBuffer | null>(null);
  const [conversations, setConversations] = useState<ConversationSummary[]>([]);
  const [historyVersion, setHistoryVersion] = useState(0);
  const bumpHistory = useCallback(() => setHistoryVersion((v) => v + 1), []);
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
  const restoreStartedRef = useRef(false);
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
          refreshServerStatus();
          if (data && typeof data === "object" && "nCtxTrain" in data) {
            setModelMeta(data as unknown as ModelInfo);
          }
        },
        onIndexProgress: (data) => {
          const d = data as { phase?: string; status?: IndexStatus };
          if (d.phase === "start") {
            setIndexing(true);
          } else if (d.phase === "done") {
            setIndexing(false);
            if (d.status) setIndexStatus(d.status);
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
    setRecentProjects((prev) => recordRecentProject(p, prev));
  }, []);

  const reindex = useCallback(() => {
    if (!projectPath) return;
    setIndexing(true);
    api
      .indexRebuild(projectPath)
      .then((st) => setIndexStatus(st))
      .catch(() => {})
      .finally(() => setIndexing(false));
  }, [projectPath]);

  const addMessage = useCallback((msg: ChatMessage) => {
    setMessages((prev) => [...prev, { ...msg, ts: msg.ts || Date.now() }]);
  }, []);

  const updateLastMessage = useCallback((update: MessageUpdate | ((prev: string) => string)) => {
    setMessages((prev) => {
      if (prev.length === 0) return prev;
      let idx = -1;
      for (let i = prev.length - 1; i >= 0; i--) {
        if (prev[i].role === "assistant" && prev[i].streaming) {
          idx = i;
          break;
        }
      }
      if (idx < 0) {
        for (let i = prev.length - 1; i >= 0; i--) {
          if (prev[i].role === "assistant") {
            idx = i;
            break;
          }
        }
      }
      if (idx < 0) idx = prev.length - 1;
      const last = prev[idx];
      let updated: ChatMessage;
      if (typeof update === "function") {
        updated = { ...last, content: update(last.content ?? "") };
      } else {
        const content =
          typeof update.content === "function"
            ? update.content(last.content ?? "")
            : update.content ?? last.content;
        const { content: _drop, ...rest } = update;
        updated = { ...last, ...rest, content };
      }
      if (updated === last) return prev;
      const next = [...prev];
      next[idx] = updated;
      return next;
    });
  }, []);

  const clearMessages = useCallback(() => setMessages([]), []);

  const truncateMessages = useCallback((fromIndex: number) => {
    setMessages((prev) => (fromIndex >= 0 ? prev.slice(0, fromIndex) : prev));
  }, []);

  const addActivity = useCallback((text: string, type: ActivityLogEntry["type"]) => {
    setActivityLog((prev) => [...prev.slice(-50), { text, type }]);
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

  const setTokenBudget = useCallback((v: number) => {
    const clamped = Math.max(0, Math.round(v));
    setTokenBudgetState(clamped);
    try {
      localStorage.setItem("ai-editor.tokenBudget", String(clamped));
    } catch {
      // storage unavailable — skip persistence
    }
  }, []);

  const setDecompose = useCallback((v: boolean) => setDecomposeState(v), []);

  const setPreferChatComplete = useCallback((v: boolean) => {
    setPreferChatCompleteState(v);
    try {
      localStorage.setItem("ai-editor.preferChatComplete", String(v));
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
    // A new chat is its own conversation. Stop any in-flight run first so a
    // live stream can't keep writing into the transcript we're about to clear,
    // then mint a fresh chat id so the next turn opens its own named log
    // (`sessions/<key>/<chat>.jsonl`) instead of appending to the shared
    // default chat (`sessions/<key>.jsonl`).
    void api.abortAgentExecution().catch(() => {});
    setIsProcessing(false);
    setMessages([]);
    setActivityLog([]);
    setCurrentConversation(`${projectPath}::${newChatId()}`);
    setComposerDraft("");
    setActiveFile(null);
    clearContext();
  }, [clearContext, projectPath]);

  const [loadingConversation, setLoadingConversation] = useState(false);

  const loadConversation = useCallback(
    async (conversationId: string) => {
      const { project, chatId } = splitConversationId(conversationId);
      setLoadingConversation(true);
      try {
        // `default` is a UI-only sentinel; the backend addresses the flat
        // log with a null chat id.
        const records = await api.sessionLoad(project, chatIdForInvoke(chatId));
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
    if (!isTauriRuntime()) return;
    if (!projectPath) {
      api.stopFileWatcher().catch(() => {});
      return;
    }
    api.agentSetWorkspace(projectPath).catch(() => {});
    api.contextSetSystemPrompt(buildSystemPrompt(currentMode, modelMeta?.nParams)).catch(() => {});
    api.startFileWatcher(projectPath).catch(() => {});
    setIndexing(true);
    api
      .indexRebuild(projectPath)
      .then((st) => setIndexStatus(st))
      .catch(() => {})
      .finally(() => setIndexing(false));
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
          // The API key is never written to localStorage: pull it (and the
          // authoritative endpoint/model) from the backend's active-remote file.
          // Fall back once to a legacy localStorage key so upgrades are seamless.
          const saved = await api.remoteConfigLoad();
          const apiKey = saved ? String(saved.apiKey ?? "") : legacyApiKeyFromStorage();
          const baseUrl = saved ? String(saved.baseUrl ?? lastModel.baseUrl) : lastModel.baseUrl;
          const provider = saved ? String(saved.provider ?? lastModel.provider) : lastModel.provider;
          const model = saved ? String(saved.model ?? lastModel.name) : lastModel.name;
          if (!apiKey) throw new Error("No API key was saved for the last model.");
          // Probe the provider (base URL + API key) before loading: only
          // reconnect the last remote model if the connection is actually live.
          await api.listRemoteModels({
            provider,
            baseUrl,
            apiKey,
          });
          await api.configureRemoteModel({
            provider,
            baseUrl,
            apiKey,
            model,
          });
          // Key is now safely in the backend's active-remote.json — drop the
          // legacy plaintext copy from localStorage.
          scrubLegacyApiKey();
          setModelName(model);
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

  const shellValue = useMemo<ShellValue>(
    () => ({
      theme, setTheme,
      sidebarCollapsed, toggleSidebar,
      historyPanelOpen, setHistoryPanelOpen, toggleHistoryPanel,
      projectPath, setProjectPath,
      indexStatus, indexing, reindex,
      recentProjects,
      backendConnected, setBackendConnected,
      hitlGate, setHitlGate,
    }),
    [
      theme, setTheme, sidebarCollapsed, toggleSidebar,
      historyPanelOpen, setHistoryPanelOpen, toggleHistoryPanel,
      projectPath, setProjectPath, indexStatus, indexing, reindex,
      recentProjects,
      backendConnected, setBackendConnected,
      hitlGate, setHitlGate,
    ],
  );

  const modelValue = useMemo<ModelValue>(
    () => ({
      modelName, modelProvider, setModelInfo,
      modelMeta, setModelMeta: setModelMetaSafe,
      modelLoading, modelLoadProgress, modelLoadStage, unloadModel,
      apiServerStatus,
      llmSettings, loadLlmSettings, saveLlmSettings, modelLoadParams,
      localModelConfigs, loadLocalModelConfigs, saveLocalModelConfig,
      activeLocalModelPath, activeModelConfig,
      rememberLastModel, lastModel,
      modelRestoreState, modelRestoreError, modelPromptVisible, dismissModelPrompt,
      promptModelSelection,
    }),
    [
      modelName, modelProvider, setModelInfo, modelMeta, setModelMetaSafe,
      modelLoading, modelLoadProgress, modelLoadStage, unloadModel,
      apiServerStatus, llmSettings, loadLlmSettings, saveLlmSettings, modelLoadParams,
      localModelConfigs, loadLocalModelConfigs, saveLocalModelConfig,
      activeLocalModelPath, activeModelConfig, rememberLastModel, lastModel,
      modelRestoreState, modelRestoreError, modelPromptVisible, dismissModelPrompt,
      promptModelSelection,
    ],
  );

  const agentSettingsValue = useMemo<AgentSettingsValue>(
    () => ({
      verify, setVerify,
      yolo, setYolo,
      maxSteps, setMaxSteps,
      tokenBudget, setTokenBudget,
      decompose, setDecompose,
      preferChatComplete, setPreferChatComplete,
      settings, setSettings,
    }),
    [
      verify, setVerify, yolo, setYolo, maxSteps, setMaxSteps,
      tokenBudget, setTokenBudget, decompose, setDecompose,
      preferChatComplete, setPreferChatComplete, settings, setSettings,
    ],
  );

  const chatValue = useMemo<ChatValue>(
    () => ({
      messages, addMessage, updateLastMessage, clearMessages, truncateMessages,
      isProcessing, setProcessing: setIsProcessing,
      composerDraft, setComposerDraft,
      currentMode, setMode,
      activityLog, addActivity, clearActivity,
      conversations, setConversations,
      historyVersion, bumpHistory,
      currentConversationId, setCurrentConversation,
      newChatSession, loadConversation, loadingConversation,
    }),
    [
      messages, addMessage, updateLastMessage, clearMessages, truncateMessages,
      isProcessing, setIsProcessing, composerDraft, setComposerDraft,
      currentMode, setMode, activityLog, addActivity, clearActivity,
      conversations, setConversations, historyVersion, bumpHistory,
      currentConversationId, setCurrentConversation,
      newChatSession, loadConversation, loadingConversation,
    ],
  );

  const contextPaneValue = useMemo<ContextPaneValue>(
    () => ({
      contextPaths, addContextPath, removeContextPath,
      contextState, setContextState, clearContext,
      activeFile, setActiveFile,
    }),
    [
      contextPaths, addContextPath, removeContextPath,
      contextState, setContextState, clearContext, activeFile, setActiveFile,
    ],
  );

  return (
    <ShellContext.Provider value={shellValue}>
      <ModelContext.Provider value={modelValue}>
        <AgentSettingsContext.Provider value={agentSettingsValue}>
          <ChatContext.Provider value={chatValue}>
            <ContextPaneContext.Provider value={contextPaneValue}>
              {children}
            </ContextPaneContext.Provider>
          </ChatContext.Provider>
        </AgentSettingsContext.Provider>
      </ModelContext.Provider>
    </ShellContext.Provider>
  );
}

