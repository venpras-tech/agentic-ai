import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";

import TitleBar from "./components/TitleBar";
import MenuBar from "./components/MenuBar";
import ModelBar from "./components/ModelBar";
import FileExplorer from "./components/FileExplorer";
import ProjectsPanel from "./components/ProjectsPanel";
import EditorPane from "./components/EditorPane";
import ChatPanel from "./components/ChatPanel";
import StatusBar from "./components/StatusBar";
import Tabs from "./components/Tabs";
import InterruptButton from "./components/InterruptButton";
import PermissionModal from "./components/PermissionModal";
import KnowledgePanel from "./components/KnowledgePanel";
import SettingsModal from "./components/SettingsModal";
import ConsolePanel from "./components/ConsolePanel";
import ResizeHandle from "./components/ResizeHandle";
import BackgroundTasks from "./components/BackgroundTasks";
import type { ConsoleEntry } from "./components/ConsolePanel";

import { useEngineEvents } from "./hooks/useEngineEvents";
import { useTokenStream } from "./hooks/useTokenStream";
import { uiLog } from "./lib/uiLog";
import {
  parseBackendLine,
  pushConsole,
  subscribeConsole,
} from "./lib/consoleBus";
import { listen } from "@tauri-apps/api/event";
import { EVT_CONTEXT_TRIMMED } from "./lib/events";
import {
  initialChatStatus,
  reduceChatStatus,
} from "./lib/chatStatus";
import { api, isTauriRuntime } from "./lib/ipc";
import { exportConversation } from "./lib/exportChat";
import { AGENT_SYSTEM_PROMPT } from "./lib/prompt";
import { recordsToMessages } from "./lib/session";
import type {
  BackgroundTaskInfo,
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
  QuestionRequest,
  RemoteModelConfig,
  StepEvent,
  SubtaskEvent,
  TodoUpdateEvent,
  ToolOutputEvent,
} from "./types";

const DEFAULT_PARAMS: GenParams = {
  contextSize: 4096,
  nThreads: 4,
  nGpuLayers: 0,
  temperature: 0.8,
  topP: 0.95,
  repeatPenalty: 1.15,
  maxTokens: 1024,
};

export default function App() {
  const { streams, append, clearStream, clearAll } = useTokenStream();
  const streamsRef = useRef(streams);
  streamsRef.current = streams;

  const [model, setModel] = useState<ModelInfo | null>(null);
  // GGUF path currently loaded (shown next to the Load/Unload button), plus
  // the most recent local path so it stays visible while no model is loaded.
  const [modelPath, setModelPath] = useState<string | null>(null);
  const [lastLocalPath, setLastLocalPath] = useState<string | null>(null);
  const [modelLoading, setModelLoading] = useState(false);
  const [loadProgress, setLoadProgress] = useState<number | null>(null);
  const [workspaces, setWorkspaces] = useState<string[]>([]);
  const workspaceRoot = workspaces[0] ?? null;
  const [files, setFiles] = useState<OpenFile[]>([]);
  const filesRef = useRef(files);
  filesRef.current = files;
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  // Latest messages for event handlers (avoids stale-closure session lookups).
  const messagesRef = useRef(messages);
  messagesRef.current = messages;
  const [activeSessionId, setActiveSessionId] = useState<number | null>(null);
  const [isStreaming, setIsStreaming] = useState(false);
  const [lastDone, setLastDone] = useState<InferenceDone | null>(null);
  const [genParams, setGenParams] = useState<GenParams>(DEFAULT_PARAMS);
  const [error, setError] = useState<string | null>(null);
  const [usage, setUsage] = useState<ContextUsage | null>(null);
  const [agentMode, setAgentMode] = useState(true);
  const [permissionReq, setPermissionReq] = useState<PermissionRequest | null>(null);
  const [questionReq, setQuestionReq] = useState<QuestionRequest | null>(null);
  const [fileChangeNotice, setFileChangeNotice] = useState(false);
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
  const [yolo, setYolo] = useState(false);
  const [attachments, setAttachments] = useState<
    { path: string; chunkCount: number }[]
  >([]);
  const [explorerRefresh, setExplorerRefresh] = useState(0);
  const [recentModels, setRecentModels] = useState<string[]>([]);
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
  // Adjustable pane sizes (drag the separator strips). Defaults mirror the
  // old fixed classes: sidebar w-60, chat w-96, console h-48.
  const [sidebarW, setSidebarW] = useState(240);
  const [chatW, setChatW] = useState(384);
  const [consoleH, setConsoleH] = useState(192);
  const paneStarts = useRef({ sidebar: 240, chat: 384, console: 192 });
  const [consoleEntries, setConsoleEntries] = useState<ConsoleEntry[]>([]);
  const [todos, setTodos] = useState<TodoUpdateEvent | null>(null);
  // BN-11 projects/chats sidebar: which left-rail view is active and which
  // chat (per project) the composer writes into. null chat id = the default
  // (legacy) chat log.
  const [leftView, setLeftView] = useState<"files" | "chats">("files");
  const [activeChatId, setActiveChatId] = useState<string | null>(null);
  const [chatsRefresh, setChatsRefresh] = useState(0);
  // Chat turn lifecycle for the animated status line (lib/chatStatus.ts).
  const [chatStatus, dispatchChatStatus] = useReducer(
    reduceChatStatus,
    initialChatStatus,
  );
  // P2-12: background tasks running independently of the foreground chat.
  const [backgroundTasks, setBackgroundTasks] = useState<BackgroundTaskInfo[]>([]);
  const bgSessionIdsRef = useRef(new Set<number>());
  const [isSwitching, setIsSwitching] = useState(false);
  const [contextTrimNotice, setContextTrimNotice] = useState(false);
  const settingsSaveTimerRef = useRef<number | null>(null);
  const fileChangeTimerRef = useRef<number | null>(null);
  const contextTrimTimerRef = useRef<number | null>(null);

  const agentModeRef = useRef(agentMode);
  agentModeRef.current = agentMode;
  const verifyRef = useRef(verify);
  verifyRef.current = verify;
  const isStreamingRef = useRef(isStreaming);
  isStreamingRef.current = isStreaming;
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
      if (!filesRef.current.some((f) => f.path && f.path.replaceAll("\\", "/") === path)) {
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
    [],
  );

  useEngineEvents({
    onToken: (e) => {
      // Background sessions accumulate silently — no chat status updates.
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      append(e.sessionId, e.delta);
      dispatchChatStatus({
        type: "token",
        sessionId: e.sessionId,
        len: e.delta.length,
        at: performance.now(),
      });
    },
    onStarted: (e) => {
      // Background tasks get their own lifecycle via onBgTask; skip chat UI.
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      setActiveSessionId(e.sessionId);
      setIsStreaming(true);
      setError(null);
      setCurrentStep(null);
      sessionStartRef.current.set(e.sessionId, performance.now());
      dispatchChatStatus({ type: "started", sessionId: e.sessionId, at: performance.now() });
      setMessages((prev) => [
        ...prev,
        { role: "assistant", content: "", sessionId: e.sessionId },
      ]);
    },
    onDone: (e) => {
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      const text = streamsRef.current.get(e.sessionId) ?? "";
      setMessages((prev) =>
        prev.map((m) =>
          m.sessionId === e.sessionId
            ? { ...m, content: text, done: e.done, ts: Date.now() }
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
        }, activeChatId)
        .then(() => setChatsRefresh((n) => n + 1))
        .catch(() => {});
      dispatchChatStatus({ type: "done", sessionId: e.sessionId, at: performance.now() });
    },
    onError: (e) => {
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      const text = streamsRef.current.get(e.sessionId) ?? "";
      const body = `${text}${text ? "\n" : ""}⚠ ${e.message}`;
      setMessages((prev) =>
        prev.map((m) =>
          m.sessionId === e.sessionId
            ? { ...m, content: body, role: "error" as const, ts: Date.now() }
            : m,
        ),
      );
      clearStream(e.sessionId);
      setActiveSessionId(null);
      setIsStreaming(false);
      setError(e.message);
      dispatchChatStatus({ type: "error", message: e.message, at: performance.now() });
      // Persist the failed turn too so restored chats show what went wrong.
      api
        .sessionAppend(workspaceRoot ?? "default", {
          role: "error",
          content: body,
          ts: Date.now(),
        }, activeChatId)
        .then(() => setChatsRefresh((n) => n + 1))
        .catch(() => {});
    },
    onTool: (e) => {
      if (e.sessionId != null && bgSessionIdsRef.current.has(e.sessionId)) return;
      dispatchChatStatus({
        type: "tool",
        tool: e.tool,
        status: e.status,
        summary: e.summary,
        at: performance.now(),
      });
      // Prefer the event's own session id when a matching turn exists; fall
      // back to the active session so late events still land.
      const targetSessionId = (() => {
        if (e.sessionId != null) {
          const hasTurn = messagesRef.current.some(
            (m) => m.sessionId === e.sessionId,
          );
          if (hasTurn) return e.sessionId;
        }
        return activeSessionId;
      })();
      if (targetSessionId == null) return;
      const sid = targetSessionId;
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
                  : [
                      ...(m.tools ?? []),
                      {
                        ...e,
                        // Anchor the call at the current end of the streamed
                        // text so the UI can interleave it inline.
                        atChar: (streamsRef.current.get(sid) ?? "").length,
                      },
                    ],
              }
            : m,
        ),
      );
    },
    onStep: (e: StepEvent) => {
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      setCurrentStep(e.step.step);
      sessionHasStepsRef.current.set(e.sessionId, true);
      dispatchChatStatus({
        type: "step",
        sessionId: e.sessionId,
        step: e.step.step,
        group: e.step.group,
        at: performance.now(),
      });
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
      dispatchChatStatus({
        type: "subtask",
        index: e.subtask.index,
        total: e.subtask.total,
        title: e.subtask.title,
        status: e.subtask.status,
        at: performance.now(),
      });
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
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
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
    onTodoUpdate: (e: TodoUpdateEvent) => {
      setTodos(e);
    },
    onBgTask: (e) => {
      if (e.status === "started") {
        bgSessionIdsRef.current.add(e.sessionId);
        setBackgroundTasks((prev) => [
          ...prev,
          {
            id: e.taskId,
            sessionId: e.sessionId,
            label: e.label,
            status: "running",
            startedAt: Date.now(),
          },
        ]);
      } else {
        // completed, error, or aborted — remove from tracking
        bgSessionIdsRef.current.delete(e.sessionId);
        setBackgroundTasks((prev) => prev.filter((t) => t.id !== e.taskId));
      }
    },
    onToolOutput: (e: ToolOutputEvent) => {
      setConsoleEntries((prev) => [
        ...prev,
        { tool: e.tool, stream: e.stream, chunk: e.chunk, ts: Date.now() },
      ]);
      if (activeSessionId == null) return;
      const sid = activeSessionId;
      if (bgSessionIdsRef.current.has(sid)) return;
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
      setExplorerRefresh((n) => n + 1);
      if (e.diff && activeSessionId != null && !bgSessionIdsRef.current.has(activeSessionId)) {
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
    onAborted: (e) => {
      setError(e.message);
      dispatchChatStatus({ type: "error", message: e.message, at: performance.now() });
    },
    onPermission: (e) => {
      setPermissionReq(e);
      dispatchChatStatus({ type: "permission", tool: e.tool, at: performance.now() });
    },
    onQuestion: (e) => {
      setQuestionReq(e);
      dispatchChatStatus({ type: "permission", tool: "ask_question", at: performance.now() });
    },
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
    onWorkspaceChanged: () => {
      // External filesystem change detected — refresh the file explorer.
      setExplorerRefresh((n) => n + 1);
      // Show a subtle notice when files change during an active agent task.
      if (isStreamingRef.current) {
        setFileChangeNotice(true);
        if (fileChangeTimerRef.current != null) {
          window.clearTimeout(fileChangeTimerRef.current);
        }
        fileChangeTimerRef.current = window.setTimeout(() => {
          setFileChangeNotice(false);
          fileChangeTimerRef.current = null;
        }, 3000);
      }
    },
  });

  // Mirror backend/LLM console lines into the in-app Console window, and
  // feed every console line (BE/LLM/UI) into the ConsolePanel entries.
  useEffect(() => {
    if (!isTauriRuntime()) return;
    let alive = true;
    // Replay history first (startup banners, model auto-load), then attach
    // the live listener — new lines after this point stream in real time.
    api
      .consoleHistory()
      .then((lines) => {
        if (!alive) return;
        lines.forEach((l) => pushConsole(parseBackendLine(l)));
        uiLog("console bridge ready — live log streaming active");
      })
      .catch(() => {});
    const unlisten = listen<string>("console-log", (e) =>
      pushConsole(parseBackendLine(e.payload)),
    ).catch(() => () => {});
    const unsubscribe = subscribeConsole((line) => {
      setConsoleEntries((prev) => {
        // Cap the ring so long agent runs can't grow memory unbounded.
        const next: ConsoleEntry[] = [
          ...prev,
          {
            tool: line.tool,
            chunk: line.chunk,
            ts: line.ts,
          },
        ];
        return next.length > 800 ? next.slice(next.length - 800) : next;
      });
    });
    return () => {
      alive = false;
      unlisten.then((fn) => fn());
      unsubscribe();
    };
  }, []);

  // Subscribe to context-trim notifications so the UI surfaces when the
  // backend prunes history to fit the context window.
  useEffect(() => {
    if (!isTauriRuntime()) return;
    const unlisten = listen(EVT_CONTEXT_TRIMMED, () => {
      setContextTrimNotice(true);
      if (contextTrimTimerRef.current != null) {
        window.clearTimeout(contextTrimTimerRef.current);
      }
      contextTrimTimerRef.current = window.setTimeout(() => {
        setContextTrimNotice(false);
        contextTrimTimerRef.current = null;
      }, 6000);
    }).catch(() => () => {});
    return () => {
      unlisten.then((fn) => fn());
      if (contextTrimTimerRef.current != null) {
        window.clearTimeout(contextTrimTimerRef.current);
        contextTrimTimerRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) {
      setError(
        "Not running inside the Tauri desktop shell. Launch with `npm run tauri:dev` — the browser preview has no Rust backend.",
      );
    }
    api
      .modelStatus()
      .then(async (m) => {
        setModel(m);
        // Root-cause fix for "chat does nothing": restore the last model (or
        // auto-detect one in ./models) instead of erroring on every prompt
        // with "No model loaded" until the user re-picks a file by hand.
        if (!m) {
          const info = await api.autoLoadModel().catch((e: unknown) => {
            uiLog("auto-load failed:", e);
            return null;
          });
          if (info) {
            setModel(info);
            api
              .contextSetSystemPrompt(AGENT_SYSTEM_PROMPT)
              .then(setUsage)
              .catch(() => {});
            refreshUsage();
          }
        } else {
          api
            .contextSetSystemPrompt(AGENT_SYSTEM_PROMPT)
            .then(setUsage)
            .catch(() => {});
        }
      })
      .catch(() => {});
    api
      .settingsLoad()
      .then((s) => {
        const params = s["params"] as Partial<GenParams> | undefined;
        if (params) setGenParams((prev) => ({ ...prev, ...params }));
        const remote = s["remote"] as RemoteModelConfig | undefined;
        if (remote) setSavedRemote(remote);
        const rm = s["recentModels"];
        if (Array.isArray(rm)) setRecentModels(rm);
      })
      .catch(() => {});
    refreshPolicy();
    return () => {
      clearAll();
      if (fileChangeTimerRef.current != null) {
        window.clearTimeout(fileChangeTimerRef.current);
        fileChangeTimerRef.current = null;
      }
    };
  }, [clearAll, refreshPolicy]);

  // ---- chat hydration: restore the last workspace + open chat on startup ----
  // The JSONL logs persist on disk; without this effect the view starts empty
  // until the user manually re-picks the workspace ("history lost on restart").
  const hydrateRef = useRef(false);
  useEffect(() => {
    if (!isTauriRuntime() || hydrateRef.current) return;
    hydrateRef.current = true;
    api
      .settingsLoad()
      .then((s) => {
        const ws = s["lastWorkspace"];
        if (typeof ws !== "string" || !ws) return;
        // Restore multi-root workspaces if saved, otherwise just the primary.
        const savedAll = s["lastWorkspaces"];
        const restoreAll = Array.isArray(savedAll) && savedAll.length > 0
          ? savedAll
          : [ws];
        const primary = restoreAll[0];
        // Use the IPC to restore all workspaces.
        return api.agentAddWorkspace(primary)
          .then(async (all) => {
            // Add any additional saved workspaces not already present.
            for (const extra of restoreAll.slice(1)) {
              if (!all.includes(extra)) {
                all = await api.agentAddWorkspace(extra).catch(() => all);
              }
            }
            setWorkspaces(all);
          })
          .then(() => {
            const chat = s["lastChat"] as
              | { project?: string; chatId?: string | null }
              | undefined;
            if (chat?.chatId && chat.project === ws && !isStreamingRef.current) {
              clearChat();
              setActiveChatId(chat.chatId);
              loadSessionIntoView(ws, chat.chatId);
            }
          });
      })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one-shot mount hydration
  }, []);

  // Persist the current workspace/chat pointer + tunable params. Debounced so
  // rapid workspace/chat/params changes don't fire overlapping read-modify-
  // write races; flushed on unmount.
  const debouncedSaveSettings = useCallback(() => {
    if (settingsSaveTimerRef.current != null) {
      window.clearTimeout(settingsSaveTimerRef.current);
    }
    settingsSaveTimerRef.current = window.setTimeout(() => {
      settingsSaveTimerRef.current = null;
      if (!isTauriRuntime()) return;
      api
        .settingsLoad()
        .then((s) =>
          api.settingsSave({
            ...s,
            lastWorkspace: workspaceRoot ?? s["lastWorkspace"],
            lastWorkspaces: workspaces,
            lastChat: { project: workspaceRoot ?? "default", chatId: activeChatId },
            params: genParams,
          }),
        )
        .catch(() => {});
    }, 500);
  }, [workspaceRoot, workspaces, activeChatId, genParams]);

  useEffect(() => {
    debouncedSaveSettings();
    return () => {
      if (settingsSaveTimerRef.current != null) {
        window.clearTimeout(settingsSaveTimerRef.current);
        settingsSaveTimerRef.current = null;
        if (isTauriRuntime()) {
          api
            .settingsLoad()
            .then((s) =>
              api.settingsSave({
                ...s,
                lastWorkspace: workspaceRoot ?? s["lastWorkspace"],
                lastWorkspaces: workspaces,
                lastChat: { project: workspaceRoot ?? "default", chatId: activeChatId },
                params: genParams,
              }),
            )
            .catch(() => {});
        }
      }
    };
  }, [debouncedSaveSettings, workspaceRoot, workspaces, activeChatId, genParams]);

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
      if (info) {
        setModel(info);
        const p = await api.loadedModelPath().catch(() => null);
        if (p) {
          setRecentModels((prev) => {
            const next = [p, ...prev.filter((x) => x !== p)].slice(0, 10);
            api.settingsLoad().then((s) =>
              api.settingsSave({ ...s, recentModels: next }),
            ).catch(() => {});
            return next;
          });
        }
      }
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
    if (messages.length > 0) {
      const ok = window.confirm(
        "Unload the current model?\n\nThis clears the visible conversation and cannot be undone.",
      );
      if (!ok) return;
    }
    try {
      await api.unloadModel();
      setModel(null);
      setModelPath(null);
      setMessages([]);
      setLastDone(null);
    } catch (e) {
      setError(String(e));
    }
  }, [messages.length]);

  /** Switch to a different local GGUF model: unload current, then load new. */
  const switchModel = useCallback(async (newPath: string) => {
    try {
      setModelLoading(true);
      setLoadProgress(0);
      if (model) await api.unloadModel();
      setModel(null);
      setModelPath(null);
      const info = await api.loadModelFromPath(newPath);
      if (info) {
        setModel(info);
        setRecentModels((prev) => {
          const next = [newPath, ...prev.filter((p) => p !== newPath)].slice(0, 10);
          // Persist to settings.
          api.settingsLoad().then((s) =>
            api.settingsSave({ ...s, recentModels: next }),
          ).catch(() => {});
          return next;
        });
      }
      api.contextSetSystemPrompt(AGENT_SYSTEM_PROMPT).then(setUsage).catch(() => {});
      refreshUsage();
    } catch (e) {
      setError(String(e));
    } finally {
      setModelLoading(false);
      setLoadProgress(null);
    }
  }, [model, refreshUsage]);

  // Keep the displayed GGUF path in sync with the backend's load state.
  useEffect(() => {
    if (!isTauriRuntime()) return;
    api
      .loadedModelPath()
      .then((p) => {
        setModelPath(p);
        if (p) setLastLocalPath(p);
      })
      .catch(() => {});
  }, [model]);

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

  const respondQuestion = useCallback(async (requestId: string, answer: string) => {
    setQuestionReq(null);
    try {
      await api.agentRespondQuestion(requestId, answer);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const abortBackgroundTask = useCallback(async (taskId: string) => {
    try {
      await api.abortBackgroundTask(taskId);
    } catch (e) {
      setError(String(e));
    }
  }, []);

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
          // Defer setActiveKey to avoid calling setState inside another
          // setState updater (React anti-pattern that can cause incorrect
          // state selection).
          queueMicrotask(() => setActiveKey(neighbor ? neighbor.id : null));
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

  const openFilePicker = useCallback(async () => {
    const path = await api.pickTextFile();
    if (path) openFile(path);
  }, [openFile]);

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
      if (e.key === "Escape") {
        if (showSettings || showKnowledge || permissionReq != null) {
          e.preventDefault();
          if (showSettings) setShowSettings(false);
          if (showKnowledge) setShowKnowledge(false);
          if (permissionReq != null) setPermissionReq(null);
          return;
        }
        if (isStreamingRef.current) {
          e.preventDefault();
          void abortAgentExecution();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [saveActive, openFilePicker, loadModel, abortAgentExecution, showSettings, showKnowledge, permissionReq]);

  /** Replay one project chat's JSONL log into the chat view + model context. */
  const loadSessionIntoView = useCallback(
    (project: string, chatId: string | null) => {
      setIsSwitching(true);
      api
        .sessionLoad(project, chatId)
        .then((records) => {
          const replay = recordsToMessages(records as never[]);
          setMessages(replay);
          for (const m of replay) {
            api
              .contextPushTurn(m.role, m.content)
              .then(setUsage)
              .catch(() => {});
          }
        })
        .catch(() => {})
        .finally(() => setIsSwitching(false));
    },
    [],
  );

  /** Make `root` the active workspace: sync the agent, refresh panels and
   * open its default chat. Shared by the folder picker and the chats tree. */
  const applyWorkspace = useCallback(
    async (root: string) => {
      setIsSwitching(true);
      try {
        const all = await api.agentAddWorkspace(root).catch(() => [root]);
        setWorkspaces(all);
        setActiveChatId(null);
        // Start file watcher for auto-reload on external changes.
        api.startFileWatcher(root).catch(() => {});
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
        loadSessionIntoView(root, null);
      } finally {
        setIsSwitching(false);
      }
    },
    [refreshPolicy, loadSessionIntoView],
  );

  const selectWorkspace = useCallback(async () => {
    const root = await api.pickWorkspaceFolder();
    if (root) await applyWorkspace(root);
  }, [applyWorkspace]);

  /** Add an additional workspace root (multi-root). */
  const addWorkspace = useCallback(async () => {
    const root = await api.pickWorkspaceFolder();
    if (!root || workspaces.includes(root)) return;
    const all = await api.agentAddWorkspace(root).catch(() => [...workspaces, root]);
    setWorkspaces(all);
    refreshPolicy();
  }, [workspaces, refreshPolicy]);

  /** Remove a workspace root. Cannot remove the primary. */
  const removeWorkspace = useCallback(async (root: string) => {
    if (workspaces.length <= 1) return;
    const all = await api.agentRemoveWorkspace(root).catch(() => workspaces.filter(w => w !== root));
    setWorkspaces(all);
    refreshPolicy();
  }, [workspaces, refreshPolicy]);

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
        repeatPenalty: genParams.repeatPenalty,
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
      dispatchChatStatus({ type: "submit", at: performance.now() });
      setPendingPlan(null);
      setCurrentSubtask(null);
      // @-mentions activate the referenced skills for this turn (and beyond).
      const mentions = [
        ...new Set(
          Array.from(trimmed.matchAll(/(?:^|\s)@([\w-]+)/g), (m) => m[1]),
        ),
      ];
      for (const name of mentions) {
        if (knowledge?.skills.some((s) => s.name === name && !s.active)) {
          api
            .skillSetActive(name, true)
            .then(setKnowledge)
            .catch(() => {});
        }
      }
      const userTs = Date.now();
      setMessages((prev) => [...prev, { role: "user", content: trimmed, ts: userTs }]);
      api
        .contextPushTurn("user", trimmed)
        .then(setUsage)
        .catch(() => {});
      api
        .sessionAppend(workspaceRoot ?? "default", {
          role: "user",
          content: trimmed,
          ts: userTs,
        }, activeChatId)
        .catch(() => {});
      try {
        if (text.startsWith("/bg ")) {
          // Background task: runs independently, does not block the chat.
          const bgPrompt = text.slice(4).trim();
          if (!bgPrompt) return;
          const bgLabel = bgPrompt.slice(0, 48);
          const sessionId = await api.agentRunBackground({
            prompt: bgPrompt,
            maxTokens: genParams.maxTokens,
            temperature: genParams.temperature,
            topP: genParams.topP,
            repeatPenalty: genParams.repeatPenalty,
            maxSteps: 6,
            stopWords: ["</s>"],
            planMode: false,
            verify: verifyRef.current,
            decompose: false,
          });
          sessionLabelRef.current.set(sessionId, bgLabel);
          bgSessionIdsRef.current.add(sessionId);
          setBackgroundTasks((prev) => [
            ...prev,
            { id: `bg-pending-${sessionId}`, sessionId, label: bgLabel, status: "running", startedAt: Date.now() },
          ]);
          setMessages((prev) => [
            ...prev,
            { role: "assistant", content: `Background task started: ${bgLabel}` },
          ]);
          return;
        }
        if (agentModeRef.current || opts?.planMode || opts?.decompose) {
          await runAgentTask(trimmed, opts);
        } else {
          await api.streamInference({
            prompt: trimmed,
            maxTokens: genParams.maxTokens,
            temperature: genParams.temperature,
            topP: genParams.topP,
            repeatPenalty: genParams.repeatPenalty,
            stopWords: ["<|endoftext|>", "\n\n---", "User:"],
          });
        }
      } catch (e) {
        const message = String(e);
        setError(message);
        dispatchChatStatus({ type: "error", message, at: performance.now() });
      }
    },
    [activeChatId, genParams, isStreaming, runAgentTask, knowledge],
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
    setTodos(null);
    dispatchChatStatus({ type: "reset", at: performance.now() });
  }, []);

  // Edit & resubmit: truncate messages from the edited index, push the
  // updated user message, and re-invoke the agent loop.
  const editResubmit = useCallback(
    async (newText: string, messageIndex: number) => {
      if (isStreaming) return;
      const trimmed = newText.trim();
      if (!trimmed) return;

      // Fork: create a new chat branching from the edit point.
      // Messages up to and including messageIndex are preserved in the new branch.
      const forkId = `chat-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
      setMessages((prev) => {
        // Take messages up to (and including) the edited message.
        const forkPrefix = prev.slice(0, messageIndex + 1);
        // Replace the edited message content.
        const lastMsg = forkPrefix[forkPrefix.length - 1];
        if (lastMsg && lastMsg.role === "user") {
          forkPrefix[forkPrefix.length - 1] = { ...lastMsg, content: trimmed };
        }
        // Persist the prefix to the new chat's JSONL.
        const ws = workspaceRoot ?? "default";
        for (const msg of forkPrefix) {
          const record: Record<string, unknown> = {
            role: msg.role,
            content: msg.content,
          };
          if (msg.ts) record.ts = msg.ts;
          if (msg.done) record.done = msg.done;
          api.sessionAppend(ws, record, forkId).catch(() => {});
        }
        return forkPrefix;
      });
      setActiveChatId(forkId);
      setLastDone(null);
      setCurrentStep(null);

      // Push the prefix messages into the model context.
      // (We need to re-push since the context was cleared for the fork.)
      const forkPrefix = messages.slice(0, messageIndex);
      for (const msg of forkPrefix) {
        api.contextPushTurn(msg.role, msg.content).catch(() => {});
      }

      // Push the edited user message.
      api.contextPushTurn("user", trimmed).then(setUsage).catch(() => {});

      dispatchChatStatus({ type: "submit", at: performance.now() });
      try {
        if (agentModeRef.current) {
          await runAgentTask(trimmed);
        } else {
          await api.streamInference({
            prompt: trimmed,
            maxTokens: genParams.maxTokens,
            temperature: genParams.temperature,
            topP: genParams.topP,
            repeatPenalty: genParams.repeatPenalty,
            stopWords: ["</s>", "\n\n---", "User:"],
          });
        }
      } catch (e) {
        const msg = String(e);
        setError(msg);
        dispatchChatStatus({ type: "error", message: msg, at: performance.now() });
      }
    },
    [activeChatId, genParams, isStreaming, messages, runAgentTask, workspaceRoot],
  );

  // Export conversation via the proper export library (PDF / DOCX / CSV).
  const [exportFormat, setExportFormat] = useState<"pdf" | "docx" | "csv">("pdf");

  const exportChat = useCallback(async () => {
    if (messages.length === 0) return;
    const firstUserMsg = messages.find((m) => m.role === "user");
    const title = firstUserMsg
      ? firstUserMsg.content.slice(0, 60).replace(/\n/g, " ")
      : "Chat Export";
    try {
      await exportConversation({ messages, title, format: exportFormat });
    } catch (err) {
      console.error("Export failed:", err);
    }
  }, [messages, exportFormat]);

  // ---- BN-11: projects/chats sidebar ----
  const newChat = useCallback(() => {
    if (isStreaming) return;
    clearChat();
    setActiveChatId(`chat-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`);
    setError(null);
    setCurrentSubtask(null);
  }, [clearChat, isStreaming]);

  const switchChat = useCallback(
    (project: string, chatId: string | null) => {
      if (isStreaming) return;
      if (project !== (workspaceRoot ?? "default")) {
        // Opening a chat from another project switches the workspace too.
        void applyWorkspace(project).then(() => {
          if (chatId) {
            clearChat();
            setActiveChatId(chatId);
            loadSessionIntoView(project, chatId);
          }
        });
        return;
      }
      if (chatId === activeChatId) return;
      clearChat();
      setActiveChatId(chatId);
      setError(null);
      setCurrentSubtask(null);
      loadSessionIntoView(workspaceRoot ?? "default", chatId);
    },
    [
      activeChatId,
      applyWorkspace,
      clearChat,
      isStreaming,
      loadSessionIntoView,
      workspaceRoot,
    ],
  );

  const deleteChat = useCallback(
    (project: string, chatId: string) => {
      api.sessionDeleteChat(project, chatId).catch(() => {});
      setChatsRefresh((n) => n + 1);
      // If the open chat itself was removed, fall back to the default chat.
      if (chatId === activeChatId && project === (workspaceRoot ?? "default")) {
        clearChat();
        setActiveChatId(null);
      }
    },
    [activeChatId, clearChat, workspaceRoot],
  );

  const handleParamsChange = useCallback((patch: Partial<GenParams>) => {
    setGenParams((prev) => ({ ...prev, ...patch }));
  }, []);

  const handleYoloChange = useCallback((v: boolean) => {
    setYolo(v);
    api.agentSetYolo(v).catch(() => {});
  }, []);

  const handleAttachClick = useCallback(async () => {
    try {
      const picked = await api.pickTextFile();
      if (!picked) return;
      const info = await api.agentAttachFile(picked);
      setAttachments((prev) =>
        prev.some((a) => a.path === info.path)
          ? prev
          : [...prev, { path: info.path, chunkCount: info.chunkCount }],
      );
    } catch {
      // picker cancelled / read failed — ignore silently
    }
  }, []);

  const handleDetachFile = useCallback((path: string) => {
    api.agentDetachFile(path).catch(() => {});
    setAttachments((prev) => prev.filter((a) => a.path !== path));
  }, []);

  const handleDropFiles = useCallback(async (paths: string[]) => {
    for (const p of paths) {
      try {
        const info = await api.agentAttachFile(p);
        setAttachments((prev) =>
          prev.some((a) => a.path === info.path)
            ? prev
            : [...prev, { path: info.path, chunkCount: info.chunkCount }],
        );
      } catch {
        // skip files that can't be read/attached
      }
    }
  }, []);

  const skills = useMemo(
    () => (knowledge?.skills ?? []).map((s) => s.name),
    [knowledge],
  );

  const handleExportFormatChange = useCallback((fmt: string) => {
    setExportFormat(fmt as "pdf" | "docx" | "csv");
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
        path={modelPath}
        lastPath={lastLocalPath}
        loading={modelLoading}
        progress={loadProgress}
        isStreaming={isStreaming}
        params={genParams}
        initialRemote={savedRemote}
        recentModels={recentModels}
        onParamsChange={handleParamsChange}
        onLoad={loadModel}
        onUnload={unloadModel}
        onSwitchModel={switchModel}
        onCancel={cancelInference}
        onConnectRemote={connectRemote}
      />
      <div className="flex min-h-0 flex-1">
        <nav
          aria-label="Sidebar"
          className="flex shrink-0 flex-col border-r border-border bg-panel"
          style={{ width: sidebarW, minWidth: 160, maxWidth: 520 }}
        >
          <div className="flex h-9 shrink-0 items-center gap-1 border-b border-border px-1.5">
            {(
              [
                ["files", "Files"],
                ["chats", "Chats"],
              ] as const
            ).map(([id, label]) => (
              <button
                key={id}
                onClick={() => setLeftView(id)}
                className={`rounded px-2 py-1 text-[10px] font-semibold uppercase tracking-wider transition-colors ${
                  leftView === id
                    ? "bg-accent/15 text-cyan-600"
                    : "text-zinc-500 hover:bg-zinc-100 hover:text-zinc-700"
                }`}
              >
                {label}
              </button>
            ))}
            <div className="flex-1" />
            {leftView === "files" && (
              <div className="flex items-center gap-0.5">
                <button
                  onClick={newFile}
                  title="New file"
                  className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
                >
                  +
                </button>
                <button
                  onClick={() => setShowKnowledge(true)}
                  title="Skills & rules"
                  className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
                >
                  ✦
                </button>
                <button
                  onClick={selectWorkspace}
                  title="Open workspace"
                  className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
                >
                  📁
                </button>
              </div>
            )}
          </div>
          {/* Keep the explorer mounted (preserves expansion state) even when
              the chats view is shown; `hidden` collapses it. */}
          <div
            className={
              leftView === "files" ? "flex min-h-0 flex-1 flex-col" : "hidden"
            }
          >
            <FileExplorer
              chromeless
              workspaceRoot={workspaceRoot}
              workspaces={workspaces}
              onSelectWorkspace={selectWorkspace}
              onAddWorkspace={addWorkspace}
              onRemoveWorkspace={removeWorkspace}
              onOpenFile={openFile}
              onNewFile={newFile}
              onOpenSkills={() => setShowKnowledge(true)}
              refreshSignal={explorerRefresh}
              onRefresh={() => setExplorerRefresh((n) => n + 1)}
            />
          </div>
          {leftView === "chats" && (
            <ProjectsPanel
              workspaceRoot={workspaceRoot}
              activeChatId={activeChatId}
              refreshSignal={chatsRefresh}
              onSwitchChat={switchChat}
              onNewChat={newChat}
              onDeleteChat={deleteChat}
              onOpenProject={(path) => void applyWorkspace(path)}
            />
          )}
        </nav>
        <ResizeHandle
          axis="x"
          onDragStart={() => (paneStarts.current.sidebar = sidebarW)}
          onDelta={(d) =>
            setSidebarW(Math.min(520, Math.max(160, paneStarts.current.sidebar + d)))
          }
        />
        <main className="flex min-w-0 flex-1 flex-col">
          <Tabs
            files={files}
            activeKey={activeKey}
            onSelect={setActiveKey}
            onClose={closeFile}
          />
          <div className="min-h-0 flex-1 overflow-hidden">
            <EditorPane file={activeFile} onContentChange={updateContent} />
          </div>
        </main>
        <ResizeHandle
          axis="x"
          onDragStart={() => (paneStarts.current.chat = chatW)}
          onDelta={(d) =>
            setChatW(Math.min(720, Math.max(300, paneStarts.current.chat - d)))
          }
        />
        <ChatPanel
          width={chatW}
          messages={messages}
          streams={streams}
          activeSessionId={activeSessionId}
          isStreaming={isStreaming}
          status={chatStatus}
          lastDone={lastDone}
          modelName={model?.name ?? null}
          agentMode={agentMode}
          onAgentModeChange={setAgentMode}
          onSend={sendPrompt}
          onCancel={cancelInference}
          onClear={clearChat}
          currentStep={currentStep}
          currentSubtask={currentSubtask}
          todos={todos}
          questionReq={questionReq}
          onRespondQuestion={respondQuestion}
          verify={verify}
          onVerifyChange={setVerify}
          yolo={yolo}
          onYoloChange={handleYoloChange}
          skills={skills}
          attachments={attachments}
          onAttachClick={handleAttachClick}
          onDetachFile={handleDetachFile}
          onDropFiles={handleDropFiles}
          pendingPlan={pendingPlan}
          onApprovePlan={approvePlan}
          onRejectPlan={rejectPlan}
          onOpenSkills={() => setShowKnowledge(true)}
          onEditResubmit={editResubmit}
          onExport={exportChat}
          exportFormat={exportFormat}
          onExportFormatChange={handleExportFormatChange}
          contextUsage={usage}
        />
        {isSwitching && (
          <div className="pointer-events-none absolute right-6 top-1/2 z-40 -translate-y-1/2 rounded-md border border-border bg-panel px-3 py-1.5 text-[11px] text-zinc-500 shadow-sm">
            Loading…
          </div>
        )}
        {contextTrimNotice && (
          <div className="pointer-events-none absolute bottom-24 right-4 z-50 animate-fade-in rounded-md border border-amber-300 bg-amber-50 px-3 py-1.5 text-[11px] text-amber-700 shadow-sm">
            Context trimmed — earlier history was summarized to fit the window.
          </div>
        )}
        {fileChangeNotice && (
          <div className="pointer-events-none absolute right-4 top-14 z-50 animate-fade-in rounded-md border border-border bg-panel-2 px-3 py-1.5 text-[11px] text-zinc-500 shadow-sm">
            External file changes detected — explorer refreshed
          </div>
        )}
      </div>
      {showConsole && (
        <ResizeHandle
          axis="y"
          onDragStart={() => (paneStarts.current.console = consoleH)}
          onDelta={(d) =>
            setConsoleH(Math.min(520, Math.max(96, paneStarts.current.console - d)))
          }
        />
      )}
      <ConsolePanel
        entries={consoleEntries}
        visible={showConsole}
        height={consoleH}
        onClear={() => setConsoleEntries([])}
      />
      <StatusBar
        model={model}
        workspaceRoot={workspaceRoot}
        workspaces={workspaces}
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
      <div className="pointer-events-none absolute bottom-8 right-20 z-30">
        <BackgroundTasks
          tasks={backgroundTasks}
          onAbort={abortBackgroundTask}
        />
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
        onParamsChange={handleParamsChange}
      />
    </div>
  );
}
