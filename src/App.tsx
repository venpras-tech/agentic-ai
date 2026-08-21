import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import TitleBar from "./components/TitleBar";
import MenuBar from "./components/MenuBar";
import ModelBar from "./components/ModelBar";
import FileExplorer from "./components/FileExplorer";
import EditorPane from "./components/EditorPane";
import ChatPanel from "./components/ChatPanel";
import StatusBar from "./components/StatusBar";
import Tabs from "./components/Tabs";
import InterruptButton from "./components/InterruptButton";
import PermissionModal from "./components/PermissionModal";
import KnowledgePanel from "./components/KnowledgePanel";
import SettingsModal from "./components/SettingsModal";
import ConsolePanel from "./components/ConsolePanel";
import type { ConsoleEntry } from "./components/ConsolePanel";

import { useEngineEvents } from "./hooks/useEngineEvents";
import { useTokenStream } from "./hooks/useTokenStream";
import { api, isTauriRuntime } from "./lib/ipc";
import { AGENT_SYSTEM_PROMPT } from "./lib/prompt";
import type {
  ChatMessage,
  ContextUsage,
  FileChangedEvent,
  GenParams,
  InferenceDone,
  KnowledgeReport,
  LedgerEntry,
  ModelInfo,
  OpenFile,
  PermissionRequest,
  PlanStepEvent,
  PolicySnapshot,
  RemoteModelConfig,
  StepEvent,
  SubtaskEvent,
  ToolOutputEvent,
} from "./types";

const DEFAULT_PARAMS: GenParams = {
  contextSize: 4096,
  nThreads: 4,
  nGpuLayers: 0,
  temperature: 0.8,
  topP: 0.95,
  maxTokens: 1024,
};

export default function App() {
  const { streams, append, clearStream, clearAll } = useTokenStream();
  const streamsRef = useRef(streams);
  streamsRef.current = streams;

  const [model, setModel] = useState<ModelInfo | null>(null);
  const [modelLoading, setModelLoading] = useState(false);
  const [loadProgress, setLoadProgress] = useState<number | null>(null);
  const [workspaceRoot, setWorkspaceRoot] = useState<string | null>(null);
  const [files, setFiles] = useState<OpenFile[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<number | null>(null);
  const [isStreaming, setIsStreaming] = useState(false);
  const [lastDone, setLastDone] = useState<InferenceDone | null>(null);
  const [genParams, setGenParams] = useState<GenParams>(DEFAULT_PARAMS);
  const [error, setError] = useState<string | null>(null);
  const [usage, setUsage] = useState<ContextUsage | null>(null);
  const [agentMode, setAgentMode] = useState(false);
  const [permissionReq, setPermissionReq] = useState<PermissionRequest | null>(null);
  const [policy, setPolicy] = useState<PolicySnapshot | null>(null);
  const [knowledge, setKnowledge] = useState<KnowledgeReport | null>(null);
  const [showKnowledge, setShowKnowledge] = useState(false);
  const [savedRemote, setSavedRemote] = useState<RemoteModelConfig | null>(null);
  const [currentStep, setCurrentStep] = useState<number | null>(null);
  const [currentSubtask, setCurrentSubtask] = useState<{
    index: number;
    total: number;
    title: string;
  } | null>(null);
  const [verify, setVerify] = useState(true);
  const [pendingPlan, setPendingPlan] = useState<{
    sessionId: number;
    planText: string;
  } | null>(null);
  const [ledger, setLedger] = useState<LedgerEntry[]>([]);
  const [checkpoints, setCheckpoints] = useState<
    { hash: string; subject: string; relative: string }[]
  >([]);
  const [showSettings, setShowSettings] = useState(false);
  const [showConsole, setShowConsole] = useState(false);
  const [consoleEntries, setConsoleEntries] = useState<ConsoleEntry[]>([]);

  const agentModeRef = useRef(agentMode);
  agentModeRef.current = agentMode;
  const verifyRef = useRef(verify);
  verifyRef.current = verify;
  const planSessionRef = useRef<number | null>(null);
  const planPromptRef = useRef<string | null>(null);
  const sessionStartRef = useRef<Map<number, number>>(new Map());
  const sessionLabelRef = useRef<Map<number, string>>(new Map());
  const sessionHasStepsRef = useRef<Map<number, boolean>>(new Map());

  const refreshUsage = useCallback(() => {
    api
      .contextStatus()
      .then(setUsage)
      .catch(() => {});
  }, []);

  const refreshPolicy = useCallback(() => {
    api
      .agentPolicySnapshot()
      .then(setPolicy)
      .catch(() => {});
  }, []);

  // Editor sync: when the agent writes/diffs a file that is currently open,
  // re-read it from disk so the Monaco editor shows the agent's changes.
  const syncAgentFile = useCallback(
    (e: FileChangedEvent) => {
      const path = e.path.replaceAll("\\", "/");
      if (!files.some((f) => f.path && f.path.replaceAll("\\", "/") === path)) {
        return;
      }
      void api
        .readTextFile(e.path)
        .then((data) => {
          setFiles((cur) =>
            cur.map((f) =>
              f.path &&
              f.path.replaceAll("\\", "/") === path &&
              f.content !== data.content
                ? { ...f, content: data.content, saved: true }
                : f,
            ),
          );
        })
        .catch(() => {});
    },
    [files],
  );

  useEngineEvents({
    onToken: (e) => append(e.sessionId, e.delta),
    onStarted: (e) => {
      setActiveSessionId(e.sessionId);
      setIsStreaming(true);
      setError(null);
      setCurrentStep(null);
      sessionStartRef.current.set(e.sessionId, performance.now());
      setMessages((prev) => [
        ...prev,
        { role: "assistant", content: "", sessionId: e.sessionId },
      ]);
    },
    onDone: (e) => {
      const text = streamsRef.current.get(e.sessionId) ?? "";
      setMessages((prev) =>
        prev.map((m) =>
          m.sessionId === e.sessionId
            ? { ...m, content: text, done: e.done }
            : m,
        ),
      );
      clearStream(e.sessionId);
      setActiveSessionId(null);
      setIsStreaming(false);
      setCurrentStep(null);
      setLastDone(e.done);
      setLedger((prev) => {
        const start = sessionStartRef.current.get(e.sessionId);
        const label = sessionLabelRef.current.get(e.sessionId) ?? "task";
        const hasSteps = sessionHasStepsRef.current.get(e.sessionId) ?? false;
        sessionStartRef.current.delete(e.sessionId);
        sessionLabelRef.current.delete(e.sessionId);
        sessionHasStepsRef.current.delete(e.sessionId);
        const existing = prev.find((l) => l.sessionId === e.sessionId);
        const stepTokens = existing?.tokens ?? 0;
        const entry: LedgerEntry = {
          sessionId: e.sessionId,
          label,
          tokens: hasSteps ? stepTokens : e.done.totalTokens,
          toolCalls: existing?.toolCalls ?? 0,
          elapsedMs: start != null ? Math.round(performance.now() - start) : e.done.elapsedMs,
        };
        return existing
          ? prev.map((l) => (l.sessionId === e.sessionId ? entry : l))
          : [...prev, entry];
      });
      if (planSessionRef.current === e.sessionId) {
        planSessionRef.current = null;
        setPendingPlan({ sessionId: e.sessionId, planText: text });
      }
      api
        .contextPushTurn("assistant", text)
        .then(setUsage)
        .catch(() => {});
      api
        .sessionAppend(workspaceRoot ?? "default", {
          role: "assistant",
          content: text,
          ts: Date.now(),
          done: e.done,
        })
        .catch(() => {});
    },
    onError: (e) => {
      const text = streamsRef.current.get(e.sessionId) ?? "";
      setMessages((prev) =>
        prev.map((m) =>
          m.sessionId === e.sessionId
            ? { ...m, content: `${text}${text ? "\n" : ""}⚠ ${e.message}`, role: "error" }
            : m,
        ),
      );
      clearStream(e.sessionId);
      setActiveSessionId(null);
      setIsStreaming(false);
      setError(e.message);
    },
    onTool: (e) => {
      if (activeSessionId == null) return;
      const sid = activeSessionId;
      setLedger((prev) => {
        const idx = prev.findIndex((l) => l.sessionId === sid);
        const entry: LedgerEntry = {
          sessionId: sid,
          label: sessionLabelRef.current.get(sid) ?? "task",
          tokens: idx >= 0 ? prev[idx].tokens : 0,
          toolCalls: (idx >= 0 ? prev[idx].toolCalls : 0) + 1,
          elapsedMs: idx >= 0 ? prev[idx].elapsedMs : 0,
        };
        return idx >= 0
          ? prev.map((l) => (l.sessionId === sid ? entry : l))
          : [...prev, entry];
      });
      setMessages((prev) =>
        prev.map((m) =>
          m.sessionId === sid
            ? {
                ...m,
                tools: (m.tools ?? []).some((t) => t.id === e.id)
                  ? (m.tools ?? []).map((t) =>
                      t.id === e.id ? { ...t, ...e, output: t.output } : t,
                    )
                  : [...(m.tools ?? []), e],
              }
            : m,
        ),
      );
    },
    onStep: (e: StepEvent) => {
      setCurrentStep(e.step.step);
      sessionHasStepsRef.current.set(e.sessionId, true);
      setMessages((prev) =>
        prev.map((m) =>
          m.sessionId === e.sessionId
            ? {
                ...m,
                steps: [
                  ...(m.steps ?? []),
                  {
                    step: e.step.step,
                    group: e.step.group,
                    tokens: e.step.tokens,
                    elapsedMs: e.step.elapsedMs,
                    toolCalls: e.step.toolCalls,
                  },
                ],
              }
            : m,
        ),
      );
      setLedger((prev) => {
        const sid = e.sessionId;
        const idx = prev.findIndex((l) => l.sessionId === sid);
        const entry: LedgerEntry = {
          sessionId: sid,
          label: sessionLabelRef.current.get(sid) ?? "task",
          tokens: (idx >= 0 ? prev[idx].tokens : 0) + e.step.tokens,
          toolCalls: idx >= 0 ? prev[idx].toolCalls : 0,
          elapsedMs: idx >= 0 ? prev[idx].elapsedMs : 0,
        };
        return idx >= 0
          ? prev.map((l) => (l.sessionId === sid ? entry : l))
          : [...prev, entry];
      });
    },
    onSubtask: (e: SubtaskEvent) => {
      if (e.subtask.status === "running") {
        setCurrentSubtask({
          index: e.subtask.index,
          total: e.subtask.total,
          title: e.subtask.title,
        });
      } else {
        setCurrentSubtask(null);
      }
    },
    onPlanStep: (e: PlanStepEvent) => {
      if (activeSessionId == null) return;
      const sid = activeSessionId;
      setMessages((prev) =>
        prev.map((m) => {
          if (m.sessionId !== sid) return m;
          const group = `Plan · ${e.title}`;
          const existing = m.steps ?? [];
          if (e.status === "in_progress") {
            return {
              ...m,
              steps: [
                ...existing,
                { step: existing.length + 1, group, tokens: 0, elapsedMs: 0, toolCalls: 0 },
              ],
            };
          }
          return m;
        }),
      );
    },
    onToolOutput: (e: ToolOutputEvent) => {
      setConsoleEntries((prev) => [
        ...prev,
        { tool: e.tool, stream: e.stream, chunk: e.chunk, ts: Date.now() },
      ]);
      if (activeSessionId == null) return;
      const sid = activeSessionId;
      if (e.tool !== "execute_terminal_command" && e.tool !== "run_tests") return;
      setMessages((prev) =>
        prev.map((m) => {
          if (m.sessionId !== sid) return m;
          const tools = m.tools ?? [];
          const idx = tools.findIndex(
            (t) =>
              t.status === "running" &&
              (t.tool === "execute_terminal_command" || t.tool === "run_tests"),
          );
          if (idx < 0) return m;
          const next = tools.map((t, i) => {
            if (i !== idx) return t;
            const merged = `${t.output ?? ""}${e.chunk}\n`;
            return {
              ...t,
              output: merged.length > 4000 ? merged.slice(-4000) : merged,
            };
          });
          return { ...m, tools: next };
        }),
      );
    },
    onFileChanged: (e: FileChangedEvent) => {
      syncAgentFile(e);
      if (e.diff && activeSessionId != null) {
        const sid = activeSessionId;
        setMessages((prev) =>
          prev.map((m) =>
            m.sessionId === sid
              ? { ...m, diffs: [...(m.diffs ?? []), e] }
              : m,
          ),
        );
      }
    },
    onSkillsChanged: () => {
      api
        .knowledgeScan()
        .then((k) => setKnowledge(k))
        .catch(() => {});
    },
    onAborted: (e) => setError(e.message),
    onPermission: setPermissionReq,
    onKnowledge: setKnowledge,
    onModelLoaded: setModel,
    onLoadProgress: (e) => {
      if (e.stage === "done") {
        setLoadProgress(null);
        setModelLoading(false);
      } else if (e.stage === "error") {
        setLoadProgress(null);
        setModelLoading(false);
      } else {
        setLoadProgress(e.progress);
      }
    },
  });

  useEffect(() => {
    if (!isTauriRuntime()) {
      setError(
        "Not running inside the Tauri desktop shell. Launch with `npm run tauri:dev` — the browser preview has no Rust backend.",
      );
    }
    api
      .modelStatus()
      .then((m) => setModel(m))
      .catch(() => {});
    api
      .settingsLoad()
      .then((s) => {
        const params = s["params"] as Partial<GenParams> | undefined;
        if (params) setGenParams((prev) => ({ ...prev, ...params }));
        const remote = s["remote"] as RemoteModelConfig | undefined;
        if (remote) setSavedRemote(remote);
      })
      .catch(() => {});
    refreshPolicy();
    return () => {
      clearAll();
    };
  }, [clearAll, refreshPolicy]);

  // Persist tunable params + last-used remote connection (no API key).
  useEffect(() => {
    api
      .settingsLoad()
      .then((s) => api.settingsSave({ ...s, params: genParams }))
      .catch(() => {});
  }, [genParams]);

  // ---- model actions ----
  const loadModel = useCallback(async () => {
    if (modelLoading) return;
    setModelLoading(true);
    setLoadProgress(0);
    setError(null);
    try {
      const info = await api.pickAndLoadModel({
        nGpuLayers: genParams.nGpuLayers,
        contextSize: genParams.contextSize,
        nThreads: genParams.nThreads,
      });
      if (info) setModel(info);
      api
        .contextSetSystemPrompt(AGENT_SYSTEM_PROMPT)
        .then(setUsage)
        .catch(() => {});
      refreshUsage();
    } catch (e) {
      setError(String(e));
    } finally {
      setModelLoading(false);
      setLoadProgress(null);
    }
  }, [genParams, modelLoading, refreshUsage]);

  const unloadModel = useCallback(async () => {
    try {
      await api.unloadModel();
      setModel(null);
      setMessages([]);
      setLastDone(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const connectRemote = useCallback(async (config: RemoteModelConfig) => {
    try {
      const info = await api.configureRemoteModel(config);
      setModel(info);
      setMessages([]);
      setLastDone(null);
      const persisted: RemoteModelConfig = {
        provider: config.provider,
        baseUrl: config.baseUrl,
        model: config.model,
        apiKey: "",
      };
      setSavedRemote(persisted);
      api
        .settingsLoad()
        .then((s) => api.settingsSave({ ...s, remote: persisted }))
        .catch(() => {});
      api
        .contextSetSystemPrompt(AGENT_SYSTEM_PROMPT)
        .then(setUsage)
        .catch(() => {});
      refreshUsage();
    } catch (e) {
      setError(String(e));
    }
  }, [refreshUsage]);

  const cancelInference = useCallback(async () => {
    try {
      await api.cancelInference();
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const abortAgentExecution = useCallback(async () => {
    try {
      await api.abortAgentExecution();
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const respondPermission = useCallback(
    async (requestId: string, decision: string) => {
      setPermissionReq(null);
      try {
        await api.agentRespondPermission(requestId, decision);
      } catch (e) {
        setError(String(e));
      }
    },
    [],
  );

  const refreshCheckpoints = useCallback(() => {
    api
      .gitCheckpoints()
      .then(setCheckpoints)
      .catch(() => setCheckpoints([]));
  }, []);

  const createCheckpoint = useCallback(async () => {
    try {
      const r = await api.gitCheckpoint();
      setError(null);
      refreshCheckpoints();
      const text = r.success
        ? `Checkpoint created.`
        : `Checkpoint failed: ${r.error ?? r.summary}`;
      setMessages((prev) => [
        ...prev,
        { role: "assistant", content: text },
      ]);
    } catch (e) {
      setError(String(e));
    }
  }, [refreshCheckpoints]);

  const revertToCheckpoint = useCallback(
    async (hash: string) => {
      const ok = window.confirm(
        `Hard-reset the workspace to checkpoint ${hash.slice(0, 7)}?\n\nThis discards all uncommitted changes and later commits. This cannot be undone.`,
      );
      if (!ok) return;
      try {
        const r = await api.gitRevert(hash);
        setError(null);
        refreshCheckpoints();
        const text = r.success
          ? `Reverted to checkpoint ${hash.slice(0, 8)}. Open files may be stale — save/reload to see the restored state.`
          : `Revert failed: ${r.error ?? r.summary}`;
        setMessages((prev) => [...prev, { role: "assistant", content: text }]);
      } catch (e) {
        setError(String(e));
      }
    },
    [refreshCheckpoints],
  );

  // ---- files ----
  const openFile = useCallback(
    async (path: string) => {
      const existing = files.find((f) => f.path === path);
      if (existing) {
        setActiveKey(existing.id);
        return;
      }
      try {
        const data = await api.readTextFile(path);
        const name = path.split(/[\\/]/).pop() ?? path;
        setFiles((prev) => [
          ...prev,
          { id: path, path, name, content: data.content, saved: true },
        ]);
        setActiveKey(path);
      } catch (e) {
        setError(String(e));
      }
    },
    [files],
  );

  const newFile = useCallback(() => {
    const id = `new:${Date.now()}`;
    const name = `untitled-${files.length + 1}`;
    setFiles((prev) => [...prev, { id, path: null, name, content: "", saved: false }]);
    setActiveKey(id);
  }, [files.length]);

  const closeFile = useCallback(
    (id: string) => {
      setFiles((prev) => {
        const idx = prev.findIndex((f) => f.id === id);
        const next = prev.filter((f) => f.id !== id);
        if (activeKey === id) {
          const neighbor = idx > 0 ? next[idx - 1] : next[idx];
          setActiveKey(neighbor ? neighbor.id : null);
        }
        return next;
      });
    },
    [activeKey],
  );

  const activeFile = useMemo(
    () => files.find((f) => f.id === activeKey) ?? null,
    [files, activeKey],
  );

  const updateContent = useCallback(
    (content: string) => {
      if (!activeFile) return;
      setFiles((prev) =>
        prev.map((f) =>
          f.id === activeFile.id
            ? { ...f, content, saved: f.saved && f.content === content }
            : f,
        ),
      );
    },
    [activeFile],
  );

  const saveActive = useCallback(async () => {
    if (!activeFile) return;
    try {
      if (!activeFile.path) {
        const path = await api.saveFileAs(activeFile.content);
        if (!path) return;
        const name = path.split(/[\\/]/).pop() ?? path;
        setFiles((prev) =>
          prev.map((f) =>
            f.id === activeFile.id ? { ...f, path, name, saved: true } : f,
          ),
        );
        return;
      }
      await api.writeTextFile(activeFile.path, activeFile.content);
      setFiles((prev) =>
        prev.map((f) => (f.id === activeFile.id ? { ...f, saved: true } : f)),
      );
    } catch (e) {
      setError(String(e));
    }
  }, [activeFile]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
        e.preventDefault();
        void saveActive();
      }
      if ((e.ctrlKey || e.metaKey) && e.key === "o") {
        e.preventDefault();
        void openFilePicker();
      }
      if ((e.ctrlKey || e.metaKey) && e.key === ",") {
        e.preventDefault();
        setShowSettings(true);
      }
      if ((e.ctrlKey || e.metaKey) && e.key === "`") {
        e.preventDefault();
        setShowConsole((v) => !v);
      }
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "l") {
        e.preventDefault();
        loadModel();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [saveActive, loadModel]);

  const openFilePicker = useCallback(async () => {
    const path = await api.pickTextFile();
    if (path) openFile(path);
  }, [openFile]);

  const selectWorkspace = useCallback(async () => {
    const root = await api.pickWorkspaceFolder();
    if (root) {
      setWorkspaceRoot(root);
      await api.agentSetWorkspace(root).catch(() => {});
      refreshPolicy();
      setCheckpoints([]);
      api
        .knowledgeReport()
        .then(setKnowledge)
        .catch(() => {});
      api
        .gitCheckpoints()
        .then(setCheckpoints)
        .catch(() => setCheckpoints([]));
      api
        .sessionLoad(root)
        .then((records) => {
          const replay: ChatMessage[] = [];
          for (const r of records) {
            const role = r["role"];
            const content = r["content"];
            if (typeof content !== "string") continue;
            if (role === "user") replay.push({ role: "user", content });
            else if (role === "assistant")
              replay.push({ role: "assistant", content: content || "…" });
          }
          setMessages(replay);
          for (const m of replay) {
            api
              .contextPushTurn(m.role, m.content)
              .then(setUsage)
              .catch(() => {});
          }
        })
        .catch(() => {});
    }
  }, [refreshPolicy, setCheckpoints]);

  // ---- context: active-file buffer (debounced so typing doesn't spam IPC) ----
  useEffect(() => {
    if (!activeFile) return;
    const t = window.setTimeout(() => {
      api
        .contextSetFileBuffer(activeFile.content.slice(0, 8000))
        .then(setUsage)
        .catch(() => {});
    }, 800);
    return () => window.clearTimeout(t);
  }, [activeFile?.id, activeFile?.content]);

  // ---- chat ----
  const runAgentTask = useCallback(
    async (
      text: string,
      opts?: { planMode?: boolean; verify?: boolean; decompose?: boolean },
    ) => {
      if (opts?.planMode) {
        planSessionRef.current = null;
        planPromptRef.current = text;
      }
      const sessionId = await api.agentRunTask({
        prompt: text,
        maxTokens: genParams.maxTokens,
        temperature: genParams.temperature,
        topP: genParams.topP,
        maxSteps: 6,
        stopWords: ["<|endoftext|>"],
        planMode: opts?.planMode ?? false,
        verify: opts?.verify ?? verifyRef.current,
        decompose: opts?.decompose ?? false,
      });
      sessionLabelRef.current.set(sessionId, text.slice(0, 48));
      if (opts?.planMode) planSessionRef.current = sessionId;
    },
    [genParams],
  );

  const sendPrompt = useCallback(
    async (
      text: string,
      opts?: { planMode?: boolean; verify?: boolean; decompose?: boolean },
    ) => {
      const trimmed = text.trim();
      if (!trimmed || isStreaming) return;
      setPendingPlan(null);
      setCurrentSubtask(null);
      setMessages((prev) => [...prev, { role: "user", content: trimmed }]);
      api
        .contextPushTurn("user", trimmed)
        .then(setUsage)
        .catch(() => {});
      api
        .sessionAppend(workspaceRoot ?? "default", {
          role: "user",
          content: trimmed,
          ts: Date.now(),
        })
        .catch(() => {});
      try {
        if (agentModeRef.current || opts?.planMode || opts?.decompose) {
          await runAgentTask(trimmed, opts);
        } else {
          await api.streamInference({
            prompt: trimmed,
            maxTokens: genParams.maxTokens,
            temperature: genParams.temperature,
            topP: genParams.topP,
            stopWords: ["<|endoftext|>", "\n\n---", "User:"],
          });
        }
      } catch (e) {
        setError(String(e));
      }
    },
    [genParams, isStreaming, runAgentTask],
  );

  const approvePlan = useCallback(async () => {
    const prompt = planPromptRef.current;
    setPendingPlan(null);
    if (!prompt) return;
    try {
      if (workspaceRoot) {
        await createCheckpoint();
        refreshCheckpoints();
      }
      await runAgentTask(prompt, { verify: true });
    } catch (e) {
      setError(String(e));
    }
  }, [createCheckpoint, refreshCheckpoints, runAgentTask, workspaceRoot]);

  const rejectPlan = useCallback(() => {
    planPromptRef.current = null;
    planSessionRef.current = null;
    setPendingPlan(null);
  }, []);

  const clearChat = useCallback(() => {
    setMessages([]);
    setPendingPlan(null);
    setLastDone(null);
    setCurrentStep(null);
    setLedger([]);
  }, []);

  return (
    <div className="flex h-full w-full flex-col bg-editor text-ink">
      <TitleBar />
      <MenuBar
        onOpenFolder={selectWorkspace}
        onOpenFile={openFilePicker}
        onSettings={() => setShowSettings(true)}
        onSelectModel={loadModel}
        onConsole={() => setShowConsole((v) => !v)}
        consoleVisible={showConsole}
      />
      <ModelBar
        model={model}
        loading={modelLoading}
        progress={loadProgress}
        isStreaming={isStreaming}
        params={genParams}
        initialRemote={savedRemote}
        onParamsChange={(patch) => setGenParams((prev) => ({ ...prev, ...patch }))}
        onLoad={loadModel}
        onUnload={unloadModel}
        onCancel={cancelInference}
        onConnectRemote={connectRemote}
      />
      <div className="flex min-h-0 flex-1">
        <FileExplorer
          workspaceRoot={workspaceRoot}
          onSelectWorkspace={selectWorkspace}
          onOpenFile={openFile}
          onNewFile={newFile}
          onOpenSkills={() => setShowKnowledge(true)}
        />
        <div className="flex min-w-0 flex-1 flex-col">
          <Tabs
            files={files}
            activeKey={activeKey}
            onSelect={setActiveKey}
            onClose={closeFile}
          />
          <div className="min-h-0 flex-1 overflow-hidden">
            <EditorPane file={activeFile} onContentChange={updateContent} />
          </div>
        </div>
        <ChatPanel
          messages={messages}
          streams={streams}
          activeSessionId={activeSessionId}
          isStreaming={isStreaming}
          lastDone={lastDone}
          modelName={model?.name ?? null}
          agentMode={agentMode}
          onAgentModeChange={setAgentMode}
          onSend={sendPrompt}
          onCancel={cancelInference}
          onClear={clearChat}
          currentStep={currentStep}
          currentSubtask={currentSubtask}
          verify={verify}
          onVerifyChange={setVerify}
          pendingPlan={pendingPlan}
          onApprovePlan={approvePlan}
          onRejectPlan={rejectPlan}
          onOpenSkills={() => setShowKnowledge(true)}
        />
      </div>
      <ConsolePanel
        entries={consoleEntries}
        visible={showConsole}
        onClear={() => setConsoleEntries([])}
      />
      <StatusBar
        model={model}
        workspaceRoot={workspaceRoot}
        activeFile={activeFile}
        error={error}
        usage={usage}
        knowledge={knowledge}
        ledger={ledger}
        checkpoints={checkpoints}
        onCheckpoint={createCheckpoint}
        onRevert={revertToCheckpoint}
      />
      <div className="pointer-events-none absolute bottom-8 right-4 z-30">
        <InterruptButton visible={isStreaming} onAbort={abortAgentExecution} />
      </div>
      <PermissionModal
        request={permissionReq}
        policy={policy}
        onRespond={respondPermission}
      />
      <KnowledgePanel open={showKnowledge} onClose={() => setShowKnowledge(false)} />
      <SettingsModal
        open={showSettings}
        onClose={() => setShowSettings(false)}
        params={genParams}
        onParamsChange={(patch) => setGenParams((prev) => ({ ...prev, ...patch }))}
      />
    </div>
  );
}
