import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import TitleBar from "./components/TitleBar";
import MenuBar from "./components/MenuBar";
import ModelBar from "./components/ModelBar";
import FileExplorer from "./components/FileExplorer";
import ProjectsPanel from "./components/ProjectsPanel";
import EditorPane from "./components/EditorPane";
import type { PendingDiffSection } from "./components/HunkDecorations";
import ChatPanel from "./components/ChatPanel";
import StatusBar from "./components/StatusBar";
import Tabs from "./components/Tabs";
import InterruptButton from "./components/InterruptButton";
import PermissionModal from "./components/PermissionModal";
import KnowledgePanel from "./components/KnowledgePanel";
import SettingsModal from "./components/SettingsModal";
import ConsolePanel from "./components/ConsolePanel";
import TerminalPanel from "./components/TerminalPanel";
import ResizeHandle from "./components/ResizeHandle";
import BackgroundTasks from "./components/BackgroundTasks";
import ChangesPanel from "./components/ChangesPanel";
import SessionResumePanel from "./components/SessionResumePanel";
import ThreadsPanel from "./components/ThreadsPanel";
import CommandPalette, {
  type PaletteAction,
  type PaletteSkill,
} from "./components/CommandPalette";
import type { ConsoleEntry } from "./components/ConsolePanel";
import ExecutionGraphPanel from "./components/ExecutionGraphPanel";

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
import { api, isTauriRuntime, type SessionAppendRecord } from "./lib/ipc";
import { exportConversation } from "./lib/exportChat";
import { recordsToMessages } from "./lib/session";
import { useCustomModes } from "./stores/customModes";
import { useModelStore } from "./stores/modelStore";
import { usePolicyStore } from "./stores/policyStore";
import { currentOpenFiles, useFilesStore } from "./stores/filesStore";
import { runStreaming, useAgentRunStore } from "./stores/agentRunStore";
import { useChatStatusStore } from "./stores/chatStatusStore";
import { useExecGraphStore } from "./stores/execGraphStore";
import { useTranscriptStore } from "./stores/transcriptStore";
import {
  handoffPrompt,
  nextChainStep,
  stepNeedsApproval,
  type HandoffChain,
} from "./lib/handoffChain";
import type {
  BackgroundTaskInfo,
  ContextUsage,
  FileChangedEvent,
  GenParams,
  ImageAttachment,
  KnowledgeReport,
  LedgerEntry,
  PermissionRequest,
  PlanStepEvent,
  QuestionRequest,
  RemoteModelConfig,
  StepEvent,
  SubtaskEvent,
  TodoUpdateEvent,
  ToolOutputEvent,
  Workflow,
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

/**
 * Client-generated turn UUID. Both halves of a turn (user prompt + assistant
 * answer / error) share one id; the backend uses it to make `sessionAppend`
 * idempotent, so a retried write never duplicates a recorded turn.
 */
function newTurnId(): string {
  const g = globalThis as { crypto?: { randomUUID?: () => string } };
  if (g.crypto?.randomUUID) return g.crypto.randomUUID();
  return `turn-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

export default function App() {
  const { streams, append, clearStream, clearAll } = useTokenStream();
  const streamsRef = useRef(streams);
  streamsRef.current = streams;

  // ---- model state (Zustand store, see ./stores/modelStore.ts) ----
  const model = useModelStore((s) => s.model);
  const setModel = useModelStore((s) => s.setModel);
  // GGUF path currently loaded (shown next to the Load/Unload button), plus
  // the most recent local path so it stays visible while no model is loaded.
  const modelPath = useModelStore((s) => s.modelPath);
  const setModelPath = useModelStore((s) => s.setModelPath);
  const lastLocalPath = useModelStore((s) => s.lastPath);
  const setLastLocalPath = useModelStore((s) => s.setLastPath);
  const modelLoading = useModelStore((s) => s.loading);
  const setModelLoading = useModelStore((s) => s.setLoading);
  const loadProgress = useModelStore((s) => s.progress);
  const setLoadProgress = useModelStore((s) => s.setProgress);
  const recentModels = useModelStore((s) => s.recentModels);
  const setRecentModels = useModelStore((s) => s.setRecentModels);
  const pushRecentModel = useModelStore((s) => s.pushRecentModel);
const [workspaces, setWorkspaces] = useState<string[]>([]);
const workspaceRoot = workspaces[0] ?? null;
/** User-defined agent modes for the current workspace (`.ai/modes/*.md`) —
 *  owned by the `useCustomModes` Zustand slice. */
const customModes = useCustomModes((s) => s.customModes);
const activeCustomMode = useCustomModes((s) => s.activeCustomMode);
const modeAwareSystemPrompt = useCustomModes((s) => s.modeAwareSystemPrompt);
const applyCustomMode = useCustomModes((s) => s.applyCustomMode);

/** User-defined workflows (`.ai/workflows/*.md`) invoked via `/name`. */
const [workflows, setWorkflows] = useState<Workflow[]>([]);

/** Reload custom modes whenever the workspace changes. */
useEffect(() => {
  void useCustomModes.getState().syncWithWorkspace(workspaceRoot);
  if (!isTauriRuntime() || !workspaceRoot) {
    setWorkflows([]);
    return;
  }
  api
    .workflowsLoad()
    .then(setWorkflows)
    .catch(() => setWorkflows([]));
}, [workspaceRoot]);

/**
 * Invoke a user workflow (`/name <goal>`): enforce its `allowedTools:`
 * allow-list on the backend, then build the effective prompt (directive +
 * goal) so the agent runs scoped to the workflow's tools.
 */
const invokeWorkflow = useCallback(
  async (name: string, goal: string) => {
    const wf = workflows.find((w) => w.name === name);
    if (!wf) return null;
    if (isTauriRuntime() && wf.allowedTools.length > 0) {
      await api.workflowEnforceTools(wf.name, wf.allowedTools).catch(() => {});
    }
    const directive =
      wf.systemPrompt.trim() ||
      `You are executing the \`${wf.name}\` workflow. Follow the workflow's steps carefully, verify after each change, and report what you did.`;
    return `${directive}\n\nGoal: ${goal}`;
  },
  [workflows],
);
  const files = useFilesStore((s) => s.files);
  const filesStoreOpen = useFilesStore((s) => s.openFile);
  const filesStoreNew = useFilesStore((s) => s.newFile);
  const filesStoreClose = useFilesStore((s) => s.closeFile);
  const filesUpdateContent = useFilesStore((s) => s.updateContent);
  const filesSyncSaved = useFilesStore((s) => s.syncSaved);
  const filesMarkSavedAs = useFilesStore((s) => s.markSavedAs);
  const filesMarkSaved = useFilesStore((s) => s.markSaved);
  const filesSetActive = useFilesStore((s) => s.setActive);
  const activeKey = useFilesStore((s) => s.activeKey);
  // Per-hunk resolution for authored diffs in Monaco: map of hunk key ->
  // whether it should be kept (applied). Absent keys default to "kept".
  const [hunkResolution, setHunkResolution] = useState<Record<string, boolean>>({});
  // Chat transcript, execution graph and turn-lifecycle state each live in a
  // pure reducer hosted by a Zustand store (lib/chatReducer.ts, lib/execGraph.ts,
  // lib/chatStatus.ts), so every event-handler transition is testable and the
  // UI never mutates state inline. Selectors subscribe only to slices they use.
  const messages = useTranscriptStore((s) => s.messages);
  const ledger = useTranscriptStore((s) => s.ledger);
  const dispatchTranscript = useTranscriptStore((s) => s.dispatch);
  const execGraph = useExecGraphStore((s) => s.graph);
  const dispatchExecGraph = useExecGraphStore((s) => s.dispatch);
  // Latest messages for event handlers (avoids stale-closure session lookups).
  const messagesRef = useRef(messages);
  messagesRef.current = messages;
  const activeSessionId = useAgentRunStore((s) => s.activeSessionId);
  const setActiveSessionId = useAgentRunStore((s) => s.setActiveSessionId);
  const isStreaming = useAgentRunStore((s) => s.isStreaming);
  const setIsStreaming = useAgentRunStore((s) => s.setIsStreaming);
  const lastDone = useAgentRunStore((s) => s.lastDone);
  const setLastDone = useAgentRunStore((s) => s.setLastDone);
  const [genParams, setGenParams] = useState<GenParams>(DEFAULT_PARAMS);
  const [error, setError] = useState<string | null>(null);
  const [usage, setUsage] = useState<ContextUsage | null>(null);
  const [agentMode, setAgentMode] = useState(true);
  const [permissionReq, setPermissionReq] = useState<PermissionRequest | null>(null);
  const [questionReq, setQuestionReq] = useState<QuestionRequest | null>(null);
  const [fileChangeNotice, setFileChangeNotice] = useState(false);
  const policy = usePolicyStore((s) => s.policy);
  const [knowledge, setKnowledge] = useState<KnowledgeReport | null>(null);
  const [showKnowledge, setShowKnowledge] = useState(false);
  const [savedRemote, setSavedRemote] = useState<RemoteModelConfig | null>(null);
  const currentStep = useAgentRunStore((s) => s.currentStep);
  const setCurrentStep = useAgentRunStore((s) => s.setCurrentStep);
  const currentSubtask = useAgentRunStore((s) => s.currentSubtask);
  const setCurrentSubtask = useAgentRunStore((s) => s.setCurrentSubtask);
  const runningSubtasks = useAgentRunStore((s) => s.runningSubtasks);
  const upsertSubtask = useAgentRunStore((s) => s.upsertSubtask);
  const removeSubtask = useAgentRunStore((s) => s.removeSubtask);
  const [verify, setVerify] = useState(true);
  const [yolo, setYolo] = useState(false);
  const [attachments, setAttachments] = useState<
    { path: string; chunkCount: number }[]
  >([]);
  const [explorerRefresh, setExplorerRefresh] = useState(0);
  const [pendingPlan, setPendingPlan] = useState<{
    sessionId: number;
    planText: string;
  } | null>(null);
  const [checkpoints, setCheckpoints] = useState<
    { hash: string; subject: string; relative: string }[]
  >([]);
  const [showSettings, setShowSettings] = useState(false);
  const [showConsole, setShowConsole] = useState(false);
  const [showTerminal, setShowTerminal] = useState(false);
  const [showGraph, setShowGraph] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteMode, setPaletteMode] = useState<"commands" | "files">("commands");
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
  const [leftView, setLeftView] = useState<
    "files" | "chats" | "changes" | "resume" | "threads"
  >("files");
  const [activeChatId, setActiveChatId] = useState<string | null>(null);
  const [chatsRefresh, setChatsRefresh] = useState(0);
  // Chat turn lifecycle for the animated status line (lib/chatStatus.ts).
  const chatStatus = useChatStatusStore((s) => s.status);
  const dispatchChatStatus = useChatStatusStore((s) => s.dispatch);
  // P2-12: background tasks running independently of the foreground chat.
  const [backgroundTasks, setBackgroundTasks] = useState<BackgroundTaskInfo[]>([]);
  const bgSessionIdsRef = useRef(new Set<number>());
  // Messages submitted while the agent is busy are queued in agentRunStore and
  // flushed serially once a turn ends.
  const queuedCount = useAgentRunStore((s) => s.queuedCount);
  const [isSwitching, setIsSwitching] = useState(false);
  const [contextTrimNotice, setContextTrimNotice] = useState(false);
  const settingsSaveTimerRef = useRef<number | null>(null);
  const fileChangeTimerRef = useRef<number | null>(null);
  const contextTrimTimerRef = useRef<number | null>(null);

  const agentModeRef = useRef(agentMode);
  agentModeRef.current = agentMode;
  const verifyRef = useRef(verify);
  verifyRef.current = verify;
  const planSessionRef = useRef<number | null>(null);
  const planPromptRef = useRef<string | null>(null);
  /** Active mode-handoff chains, keyed by the session id of the phase that is
   *  currently executing. On that phase's `onDone` we auto-advance to the next
   *  phase (Plan→Act→Review), carrying the prior phase's result. */
  const handoffRef = useRef<
    Map<number, { chain: HandoffChain; index: number; basePrompt: string }>
  >(new Map());
  /** Chain context for a Plan phase awaiting "Approve & Execute" approval. */
  const pendingHandoffRef = useRef<{
    chain: HandoffChain;
    index: number;
    basePrompt: string;
  } | null>(null);
  const sessionStartRef = useRef<Map<number, number>>(new Map());
  const sessionLabelRef = useRef<Map<number, string>>(new Map());
  const sessionHasStepsRef = useRef<Map<number, boolean>>(new Map());
  /** Maps a backend session id to the client turn UUID it belongs to, so the
   * assistant-answer record can carry the same `turnId` as its user prompt.
   * (This is what makes `sessionAppend` replays idempotent on the backend.) */
  const sessionTurnRef = useRef<Map<number, string>>(new Map());

  const refreshUsage = useCallback(() => {
    api
      .contextStatus()
      .then(setUsage)
      .catch(() => {});
  }, []);

  const refreshPolicy = usePolicyStore((s) => s.refreshPolicy);

  // Editor sync: when the agent writes/diffs a file that is currently open,
  // re-read it from disk so the Monaco editor shows the agent's changes.
  const syncAgentFile = useCallback(
    (e: FileChangedEvent) => {
      const path = e.path.replaceAll("\\", "/");
      if (
        !currentOpenFiles().some((f) => f.path && f.path.replaceAll("\\", "/") === path)
      ) {
        return;
      }
      void api
        .readTextFile(e.path)
        .then((data) => {
          const id = currentOpenFiles().find(
            (f) => f.path && f.path.replaceAll("\\", "/") === path,
          )?.id;
          if (!id) return;
          const cur = currentOpenFiles().find((f) => f.id === id);
          if (cur && cur.content !== data.content) {
            filesSyncSaved(id, data.content);
          }
        })
        .catch(() => {});
    },
    [filesSyncSaved],
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
      dispatchTranscript({
        type: "push",
        message: { role: "assistant", content: "", sessionId: e.sessionId },
      });
    },
    onDone: (e) => {
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      const text = streamsRef.current.get(e.sessionId) ?? "";
      const turnId = sessionTurnRef.current.get(e.sessionId);
      sessionTurnRef.current.delete(e.sessionId);
      dispatchTranscript({
        type: "mergeById",
        sessionId: e.sessionId,
        patch: { content: text, done: e.done, ts: Date.now(), ...(turnId ? { turnId } : {}) },
      });
      clearStream(e.sessionId);
      setActiveSessionId(null);
      setIsStreaming(false);
      setCurrentStep(null);
      setLastDone(e.done);
      const start = sessionStartRef.current.get(e.sessionId);
      const label = sessionLabelRef.current.get(e.sessionId) ?? "task";
      const hasSteps = sessionHasStepsRef.current.get(e.sessionId) ?? false;
      sessionStartRef.current.delete(e.sessionId);
      sessionLabelRef.current.delete(e.sessionId);
      sessionHasStepsRef.current.delete(e.sessionId);
      const entry: LedgerEntry = {
        sessionId: e.sessionId,
        label,
        tokens: hasSteps ? (ledger.find((l) => l.sessionId === e.sessionId)?.tokens ?? 0) : e.done.totalTokens,
        toolCalls: ledger.find((l) => l.sessionId === e.sessionId)?.toolCalls ?? 0,
        elapsedMs: start != null ? Math.round(performance.now() - start) : e.done.elapsedMs,
      };
      dispatchTranscript({ type: "ledgerSet", entry });
      if (planSessionRef.current === e.sessionId) {
        planSessionRef.current = null;
        setPendingPlan({ sessionId: e.sessionId, planText: text });
      }
      // Mode-handoff: a completing phase may auto-continue to the next phase.
      const handoffEntry = handoffRef.current.get(e.sessionId);
      if (handoffEntry) {
        handoffRef.current.delete(e.sessionId);
        const next = nextChainStep(handoffEntry.chain, handoffEntry.index);
        // Plan steps require explicit user approval (handled by the plan
        // approval flow) — only auto-advance to non-plan phases here.
        if (next && !stepNeedsApproval(next)) {
          const nextOpts =
            next === "review" ? { verify: true, handoff: handoffEntry.chain } : { handoff: handoffEntry.chain };
          void runAgentTaskRef.current(
            handoffPrompt(handoffEntry.basePrompt, text),
            nextOpts,
          );
        }
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
          ...(turnId ? { turnId } : {}),
        }, activeChatId)
        .then(() => setChatsRefresh((n) => n + 1))
        .catch(() => {});
      dispatchChatStatus({ type: "done", sessionId: e.sessionId, at: performance.now() });
    },
    onError: (e) => {
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      const text = streamsRef.current.get(e.sessionId) ?? "";
      const body = `${text}${text ? "\n" : ""}⚠ ${e.message}`;
      const turnId = sessionTurnRef.current.get(e.sessionId);
      sessionTurnRef.current.delete(e.sessionId);
      dispatchTranscript({
        type: "mergeById",
        sessionId: e.sessionId,
        patch: { content: body, role: "error", ts: Date.now(), ...(turnId ? { turnId } : {}) },
      });
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
          ...(turnId ? { turnId } : {}),
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
      dispatchTranscript({
        type: "ledgerTool",
        sessionId: sid,
        label: sessionLabelRef.current.get(sid) ?? "task",
      });
      dispatchTranscript({
        type: "tool",
        sessionId: sid,
        tool: {
          ...e,
          // Anchor the call at the current end of the streamed text so the
          // UI can interleave it inline.
          atChar: (streamsRef.current.get(sid) ?? "").length,
        },
      });
      // Mirror the call into the live execution graph for the active run.
      if (sid === activeSessionId) {
        dispatchExecGraph({
          type: "tool",
          id: e.id,
          name: e.tool,
          status: e.status,
          summary: e.summary,
        });
      }
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
      dispatchTranscript({
        type: "appendStep",
        sessionId: e.sessionId,
        step: {
          step: e.step.step,
          group: e.step.group,
          tokens: e.step.tokens,
          elapsedMs: e.step.elapsedMs,
          toolCalls: e.step.toolCalls,
        },
      });
      dispatchTranscript({
        type: "ledgerTokens",
        sessionId: e.sessionId,
        label: sessionLabelRef.current.get(e.sessionId) ?? "task",
        tokens: e.step.tokens,
      });
      if (e.sessionId === activeSessionId) {
        dispatchExecGraph({ type: "step", group: e.step.group });
      }
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
      dispatchExecGraph({
        type: "subtask",
        index: e.subtask.index,
        total: e.subtask.total,
        title: e.subtask.title,
        status: e.subtask.status,
      });
      if (e.subtask.status === "running") {
        const { index, total, title, model, tool } = e.subtask;
        const prev = useAgentRunStore.getState().currentSubtask;
        const same =
          prev != null &&
          prev.index === index &&
          prev.total === total &&
          prev.title === title;
        const startedAt =
          same && prev.startedAt != null
            ? prev.startedAt
            : Date.now() - (e.subtask.elapsedMs ?? 0);
        setCurrentSubtask({ index, total, title, model, tool, startedAt });
        upsertSubtask({ index, total, title, model, tool, startedAt });
      } else {
        setCurrentSubtask(null);
        removeSubtask(e.subtask.index, e.subtask.total);
      }
    },
    onPlanStep: (e: PlanStepEvent) => {
      if (activeSessionId == null) return;
      if (bgSessionIdsRef.current.has(e.sessionId)) return;
      const sid = activeSessionId;
      if (e.status === "in_progress") {
        dispatchTranscript({
          type: "appendPlanStep",
          sessionId: sid,
          group: `Plan · ${e.title}`,
        });
      }
      dispatchExecGraph({
        type: "planStep",
        planId: e.planId,
        itemIndex: e.itemIndex,
        title: e.title,
        status: e.status,
      });
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
      dispatchTranscript({ type: "appendToolOutput", sessionId: sid, chunk: e.chunk });
    },
    onFileChanged: (e: FileChangedEvent) => {
      syncAgentFile(e);
      setExplorerRefresh((n) => n + 1);
      if (e.diff && activeSessionId != null && !bgSessionIdsRef.current.has(activeSessionId)) {
        dispatchTranscript({ type: "appendDiff", sessionId: activeSessionId, diff: e });
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
      if (runStreaming()) {
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
              .contextSetSystemPrompt(modeAwareSystemPrompt())
              .then(setUsage)
              .catch(() => {});
            refreshUsage();
          }
        } else {
          api
            .contextSetSystemPrompt(modeAwareSystemPrompt())
            .then(setUsage)
            .catch(() => {});
        }
      })
      .catch(() => {});
    api
      .settingsLoad()
      .then((s) => {
        if (s.params) setGenParams((prev) => ({ ...prev, ...s.params }));
        if (s.remote) setSavedRemote(s.remote);
        const rm = s.recentModels;
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
        const ws = s.lastWorkspace;
        if (!ws) return;
        // Restore multi-root workspaces if saved, otherwise just the primary.
        const savedAll = s.lastWorkspaces;
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
            const chat = s.lastChat;
            if (chat?.chatId && chat.project === ws && !runStreaming()) {
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
            lastWorkspace: workspaceRoot ?? s.lastWorkspace,
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
                lastWorkspace: workspaceRoot ?? s.lastWorkspace,
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
          pushRecentModel(p, (next) =>
            api.settingsLoad().then((s) =>
              api.settingsSave({ ...s, recentModels: next }),
            ).catch(() => {}),
          );
        }
      }
      api
        .contextSetSystemPrompt(modeAwareSystemPrompt())
        .then(setUsage)
        .catch(() => {});
      refreshUsage();
    } catch (e) {
      setError(String(e));
    } finally {
      setModelLoading(false);
      setLoadProgress(null);
    }
  }, [genParams, modelLoading, refreshUsage, pushRecentModel]);

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
      dispatchTranscript({ type: "clearMessages" });
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
        pushRecentModel(newPath, (next) =>
          api.settingsLoad().then((s) =>
            api.settingsSave({ ...s, recentModels: next }),
          ).catch(() => {}),
        );
      }
      api.contextSetSystemPrompt(modeAwareSystemPrompt()).then(setUsage).catch(() => {});
      refreshUsage();
    } catch (e) {
      setError(String(e));
    } finally {
      setModelLoading(false);
      setLoadProgress(null);
    }
  }, [model, refreshUsage, pushRecentModel]);

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
      dispatchTranscript({ type: "clearMessages" });
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
        .contextSetSystemPrompt(modeAwareSystemPrompt())
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
      dispatchTranscript({ type: "push", message: { role: "assistant", content: text } });
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
        dispatchTranscript({ type: "push", message: { role: "assistant", content: text } });
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
        filesSetActive(existing.id);
        return;
      }
      try {
        const data = await api.readTextFile(path);
        const name = path.split(/[\\/]/).pop() ?? path;
        filesStoreOpen({ id: path, path, name, content: data.content, saved: true });
      } catch (e) {
        setError(String(e));
      }
    },
    [files, filesSetActive, filesStoreOpen],
  );

  const newFile = useCallback(() => {
    const id = `new:${Date.now()}`;
    const name = `untitled-${files.length + 1}`;
    filesStoreNew({ id, path: null, name, content: "", saved: false });
  }, [files.length, filesStoreNew]);

  const closeFile = useCallback(
    (id: string) => {
      const target = files.find((f) => f.id === id);
      // Unsaved-changes guard: closing a dirty file discards unsaved edits.
      if (target && !target.saved) {
        const ok = window.confirm(
          `Close "${target.name}" without saving?\n\nUnsaved changes will be lost.`,
        );
        if (!ok) return;
      }
      filesStoreClose(id);
    },
    [files, filesStoreClose],
  );

  const activeFile = useMemo(
    () => files.find((f) => f.id === activeKey) ?? null,
    [files, activeKey],
  );

  const updateContent = useCallback(
    (content: string) => {
      if (!activeFile) return;
      filesUpdateContent(activeFile.id, content);
    },
    [activeFile, filesUpdateContent],
  );

  const saveActive = useCallback(async () => {
    if (!activeFile) return;
    try {
      if (!activeFile.path) {
        const path = await api.saveFileAs(activeFile.content);
        if (!path) return;
        const name = path.split(/[\\/]/).pop() ?? path;
        filesMarkSavedAs(activeFile.id, path, name);
        return;
      }
      await api.writeTextFile(activeFile.path, activeFile.content);
      filesMarkSaved(activeFile.id);
    } catch (e) {
      setError(String(e));
    }
  }, [activeFile, filesMarkSaved, filesMarkSavedAs]);

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
      if ((e.ctrlKey || e.metaKey) && e.altKey && e.key.toLowerCase() === "t") {
        e.preventDefault();
        toggleTerminal();
      }
      if ((e.ctrlKey || e.metaKey) && e.altKey && e.key.toLowerCase() === "g") {
        e.preventDefault();
        toggleGraph();
      }
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "l") {
        e.preventDefault();
        loadModel();
      }
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key.toLowerCase() === "p") {
        e.preventDefault();
        setPaletteMode("commands");
        setPaletteOpen(true);
      }
      if ((e.ctrlKey || e.metaKey) && !e.shiftKey && e.key.toLowerCase() === "p") {
        e.preventDefault();
        setPaletteMode("files");
        setPaletteOpen(true);
      }
      if (e.key === "Escape") {
        if (paletteOpen || showSettings || showKnowledge || permissionReq != null || questionReq != null) {
          e.preventDefault();
          if (paletteOpen) setPaletteOpen(false);
          if (showSettings) setShowSettings(false);
          if (showKnowledge) setShowKnowledge(false);
          if (permissionReq != null) setPermissionReq(null);
          if (questionReq != null) setQuestionReq(null);
          return;
        }
        if (runStreaming()) {
          e.preventDefault();
          void abortAgentExecution();
        }
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [saveActive, openFilePicker, loadModel, abortAgentExecution, showSettings, showKnowledge, permissionReq, questionReq, paletteOpen]);

  /** Replay one project chat's JSONL log into the chat view + model context. */
  const loadSessionIntoView = useCallback(
    (project: string, chatId: string | null) => {
      setIsSwitching(true);
      api
        .sessionLoad(project, chatId)
        .then((records) => {
          const replay = recordsToMessages(records);
          dispatchTranscript({ type: "replaceAll", messages: replay });
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
      opts?: {
        planMode?: boolean;
        verify?: boolean;
        decompose?: boolean;
        images?: ImageAttachment[];
        handoff?: HandoffChain;
      },
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
        agentMode: useCustomModes.getState().activeModeRef?.name ?? undefined,
        images: opts?.images,
      });
      sessionLabelRef.current.set(sessionId, text.slice(0, 48));
      if (opts?.planMode) planSessionRef.current = sessionId;
      // Register a handoff chain's current phase against this session so the
      // `onDone` handler can auto-advance. A fresh send starts at index 0.
      if (opts?.handoff) {
        if (opts.planMode) {
          // Plan phase pauses for approval; stash the continuation context.
          pendingHandoffRef.current = {
            chain: opts.handoff,
            index: 0,
            basePrompt: text,
          };
        } else {
          handoffRef.current.set(sessionId, {
            chain: opts.handoff,
            index: 0,
            basePrompt: text,
          });
        }
      }
      return sessionId;
    },
    [genParams],
  );

  const sendPrompt = useCallback(
    async (
      text: string,
      opts?: {
        planMode?: boolean;
        verify?: boolean;
        decompose?: boolean;
        images?: ImageAttachment[];
        handoff?: HandoffChain;
      },
    ) => {
      const trimmed = text.trim();
      if (!trimmed) return;
      if (text.startsWith("/fork ")) {
        // Fork: spin up an independent background task seeded with the CURRENT
        // conversation (the backend snapshots the whole ContextManager), but
        // leave the foreground transcript untouched — no user turn, no status
        // churn. Only a background pill + a marker note appear here.
        const forkPrompt = text.slice(5).trim();
        if (!forkPrompt) return;
        const forkLabel = forkPrompt.slice(0, 48);
        try {
          const sessionId = await api.agentRunBackground({
            prompt: forkPrompt,
            maxTokens: genParams.maxTokens,
            temperature: genParams.temperature,
            topP: genParams.topP,
            repeatPenalty: genParams.repeatPenalty,
            maxSteps: 12,
            stopWords: ["</s>"],
            planMode: false,
            verify: verifyRef.current,
            decompose: false,
          });
          sessionLabelRef.current.set(sessionId, forkLabel);
          bgSessionIdsRef.current.add(sessionId);
          setBackgroundTasks((prev) => [
            ...prev,
            { id: `bg-pending-${sessionId}`, sessionId, label: forkLabel, status: "running", startedAt: Date.now() },
          ]);
          dispatchTranscript({
            type: "push",
            message: { role: "assistant", content: `Forked conversation into a background task: ${forkLabel}` },
          });
        } catch (e) {
          dispatchChatStatus({ type: "error", message: String(e), at: performance.now() });
        }
        return;
      }
      if (isStreaming) {
        // Agent is busy — queue the message and run it once the turn finishes
        // instead of silently dropping it (queued/steered messages).
        useAgentRunStore.getState().enqueuePrompt({ text: trimmed, opts });
        return;
      }
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
      // One turn UUID for both halves (user prompt + assistant answer), so the
      // backend can dedupe replayed `sessionAppend` writes.
      const turnId = newTurnId();
      dispatchTranscript({
        type: "push",
        message: { role: "user", content: trimmed, ts: userTs, turnId },
      });
      api
        .contextPushTurn("user", trimmed)
        .then(setUsage)
        .catch(() => {});
      api
        .sessionAppend(workspaceRoot ?? "default", {
          role: "user",
          content: trimmed,
          ts: userTs,
          turnId,
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
          dispatchTranscript({
            type: "push",
            message: { role: "assistant", content: `Background task started: ${bgLabel}` },
          });
          return;
        }
        let sid: number | undefined;
        if (agentModeRef.current || opts?.planMode || opts?.decompose || opts?.handoff) {
          sid = await runAgentTask(trimmed, opts);
        } else {
          sid = await api.streamInference({
            prompt: trimmed,
            maxTokens: genParams.maxTokens,
            temperature: genParams.temperature,
            topP: genParams.topP,
            repeatPenalty: genParams.repeatPenalty,
            stopWords: ["<|endoftext|>", "\n\n---", "User:"],
            images: opts?.images,
          });
        }
        if (sid != null) sessionTurnRef.current.set(sid, turnId);
      } catch (e) {
        const message = String(e);
        setError(message);
        dispatchChatStatus({ type: "error", message, at: performance.now() });
      }
    },
    [activeChatId, genParams, isStreaming, runAgentTask, knowledge],
  );

  // Flush one queued message each time the agent transitions back to idle, so
  // messages submitted while busy run serially instead of being dropped.
  const sendPromptRef = useRef(sendPrompt);
  sendPromptRef.current = sendPrompt;
  // Mode-handoff continuation (onDone) calls through a ref so it always sees
  // the latest runAgentTask, never a stale render's closure.
  const runAgentTaskRef = useRef(runAgentTask);
  runAgentTaskRef.current = runAgentTask;
  useEffect(() => {
    if (isStreaming) return;
    const next = useAgentRunStore.getState().shiftPrompt();
    if (!next) return;
    void sendPromptRef.current(next.text, next.opts);
  }, [isStreaming]);

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
    dispatchTranscript({ type: "reset" });
    dispatchExecGraph({ type: "reset" });
    setPendingPlan(null);
    setLastDone(null);
    setCurrentStep(null);
    setTodos(null);
    dispatchChatStatus({ type: "reset", at: performance.now() });
  }, []);

  /** Record a per-diff accept/reject in the shared transcript store. The
   *  inline timeline and the Changes panel both dispatch here, so they never
   *  disagree about what is pending. */
  const handleDiffResolve = useCallback(
    (messageIndex: number, diffIndex: number, status: "accepted" | "rejected") => {
      dispatchTranscript({
        type: "diffResolved",
        messageIndex,
        diffIndex,
        status,
      });
    },
    [],
  );

  // Monospace-independent authored-diff sections presented to the editor for
  // per-hunk keep/revert. Only diffs that are still relevant on this file are
  // surfaced: pending (unresolved) or accepted (kept) diffs; a diff resolved
  // as "rejected" means its change was reverted and is no longer in the buffer.
  const activePath = activeFile?.path?.replaceAll("\\", "/");
  const pendingSections = useMemo<PendingDiffSection[]>(() => {
    if (!activePath) return [];
    const out: PendingDiffSection[] = [];
    messages.forEach((m, mi) => {
      (m.diffs ?? []).forEach((d, di) => {
        if (d.resolved === "rejected") return;
        if (!d.before || !d.diff) return;
        if (d.path.replaceAll("\\", "/") !== activePath) return;
        // Stable key shared with `handleDiffResolve` so per-hunk state survives
        // when the sections list reorders (e.g. an earlier diff is rejected).
        out.push({ key: `${mi}-${di}`, before: d.before, diff: d.diff });
      });
    });
    return out;
  }, [messages, activePath]);

  // Persist a per-hunk toggle: recompute the file content (done by the caller)
  // and write it to disk, sync the open buffer, and record the new keep state.
  const handleToggleHunk = useCallback(
    async (opts: {
      before: string;
      diff: string | null;
      key: string;
      keep: boolean;
      content: string;
    }) => {
      if (!activeFile?.path) return;
      setHunkResolution((prev) => ({ ...prev, [opts.key]: opts.keep }));
      try {
        await api.writeTextFile(activeFile.path, opts.content);
        filesSyncSaved(activeFile.id, opts.content);
      } catch (err) {
        // Revert the optimistic state if the write failed.
        setHunkResolution((prev) => {
          const next = { ...prev };
          delete next[opts.key];
          return next;
        });
        console.error("Failed to write per-hunk change:", err);
      }
    },
    [activeFile, filesSyncSaved],
  );

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
      const forkTurnId = newTurnId();
      const forkPrefix = messages.slice(0, messageIndex + 1);
      // Replace the edited message content.
      const lastMsg = forkPrefix[forkPrefix.length - 1];
      if (lastMsg && lastMsg.role === "user") {
        forkPrefix[forkPrefix.length - 1] = {
          ...lastMsg,
          content: trimmed,
          turnId: forkTurnId,
        };
      }
      // Persist the prefix to the new chat's JSONL (outside the state updater).
      const ws = workspaceRoot ?? "default";
      for (const msg of forkPrefix) {
        const record: SessionAppendRecord = {
          role: msg.role,
          content: msg.content,
        };
        if (msg.ts) record.ts = msg.ts;
        if (msg.done) record.done = msg.done;
        if (msg.turnId) record.turnId = msg.turnId;
        api.sessionAppend(ws, record, forkId).catch(() => {});
      }
      dispatchTranscript({ type: "replaceAll", messages: forkPrefix });
      setActiveChatId(forkId);
      setLastDone(null);
      setCurrentStep(null);

      // Push the prefix messages into the model context.
      // (We need to re-push since the context was cleared for the fork.)
      const pushPrefix = messages.slice(0, messageIndex);
      for (const msg of pushPrefix) {
        api.contextPushTurn(msg.role, msg.content).catch(() => {});
      }

      // Push the edited user message.
      api.contextPushTurn("user", trimmed).then(setUsage).catch(() => {});

      dispatchChatStatus({ type: "submit", at: performance.now() });
      let sid: number | undefined;
      try {
        if (agentModeRef.current) {
          sid = await runAgentTask(trimmed);
        } else {
          sid = await api.streamInference({
            prompt: trimmed,
            maxTokens: genParams.maxTokens,
            temperature: genParams.temperature,
            topP: genParams.topP,
            repeatPenalty: genParams.repeatPenalty,
            stopWords: ["</s>", "\n\n---", "User:"],
          });
        }
        if (sid != null) sessionTurnRef.current.set(sid, forkTurnId);
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

  const paletteSkills = useMemo<PaletteSkill[]>(
    () =>
      (knowledge?.skills ?? []).map((s) => ({ name: s.name, active: s.active })),
    [knowledge],
  );

  const handleExportFormatChange = useCallback((fmt: string) => {
    setExportFormat(fmt as "pdf" | "docx" | "csv");
  }, []);

  // Stable UI affordances (memoization-safe; passed to memoized children).
  const openSettings = useCallback(() => setShowSettings(true), []);
  const closeSettings = useCallback(() => setShowSettings(false), []);
  const toggleConsole = useCallback(() => {
    setShowTerminal(false);
    setShowConsole((v) => !v);
  }, []);
  const clearConsole = useCallback(() => setConsoleEntries([]), []);
  const toggleTerminal = useCallback(() => {
    setShowConsole(false);
    setShowTerminal((v) => !v);
  }, []);
  const toggleGraph = useCallback(() => {
    setShowConsole(false);
    setShowTerminal(false);
    setShowGraph((v) => !v);
  }, []);
  const clearGraph = useCallback(() => {
    dispatchExecGraph({ type: "reset" });
  }, []);
  const openKnowledge = useCallback(() => setShowKnowledge(true), []);
  const closeKnowledge = useCallback(() => setShowKnowledge(false), []);
  const refreshExplorer = useCallback(() => setExplorerRefresh((n) => n + 1), []);

  const paletteActions = useMemo<PaletteAction[]>(
    () => [
      {
        id: "new-chat",
        label: "New chat",
        hint: "start a fresh conversation",
        keywords: "new chat clear conversation reset",
        run: newChat,
      },
      {
        id: "clear-chat",
        label: "Clear chat",
        hint: "reset the current transcript",
        keywords: "clear chat empty reset",
        run: clearChat,
      },
      {
        id: "settings",
        label: "Open settings",
        hint: "model & generation params",
        keywords: "settings options preferences",
        run: openSettings,
      },
      {
        id: "console",
        label: "Toggle console",
        hint: "show / hide the console pane",
        keywords: "console log toggle",
        run: toggleConsole,
      },
      {
        id: "terminal",
        label: "Toggle terminal",
        hint: "open the interactive terminal pane",
        keywords: "terminal shell command toggle",
        run: toggleTerminal,
      },
      {
        id: "exec-graph",
        label: "Toggle execution graph",
        hint: "show plan · subtask · tool call graph",
        keywords: "graph execution plan subtask tool visualization",
        run: toggleGraph,
      },
      {
        id: "clear-graph",
        label: "Clear execution graph",
        hint: "reset the live run graph",
        keywords: "clear graph reset run",
        run: clearGraph,
      },
      {
        id: "clear-console",
        label: "Clear console",
        hint: "empty the console pane",
        keywords: "clear console log",
        run: clearConsole,
      },
      {
        id: "knowledge",
        label: "Open knowledge / skills",
        hint: "rules, skills, and memory",
        keywords: "skills knowledge rules memory",
        run: openKnowledge,
      },
      ...(isStreaming
        ? [
            {
              id: "cancel",
              label: "Cancel inference",
              hint: "stop the running generation",
              keywords: "cancel stop abort interrupt",
              run: () => void cancelInference(),
              danger: true,
            },
          ]
        : []),
      {
        id: "toggle-agent",
        label: agentMode ? "Disable agent mode" : "Enable agent mode",
        hint: "agentic tool use on / off",
        keywords: "agent mode toggle autonomous",
        run: () => setAgentMode(!agentMode),
      },
      {
        id: "toggle-verify",
        label: verify ? "Disable verify" : "Enable verify",
        hint: "auto-verify after changes",
        keywords: "verify toggle check run tests",
        run: () => setVerify(!verify),
      },
      {
        id: "toggle-yolo",
        label: yolo ? "Disable YOLO mode" : "Enable YOLO mode",
        hint: "auto-approve tool actions",
        keywords: "yolo auto approve trust skip",
        run: () => handleYoloChange(!yolo),
        danger: !yolo,
      },
      {
        id: "open-file",
        label: "Open file…",
        hint: "choose a file via dialog",
        keywords: "open file picker browse",
        run: () => void openFilePicker(),
      },
      {
        id: "changes-panel",
        label: "Show file changes",
        hint: "open the Changes panel (working-tree diffs)",
        keywords: "changes panel diff source control review accept revert",
        run: () => setLeftView("changes"),
      },
      {
        id: "resume-session",
        label: "Resume a session…",
        hint: "re-open a previous chat across projects",
        keywords: "resume session restore continue history reopen picker",
        run: () => setLeftView("resume"),
      },
      {
        id: "threads-panel",
        label: "Open threads / status",
        hint: "live agent run + background task status",
        keywords: "threads tasks sessions status background agent running",
        run: () => setLeftView("threads"),
      },
    ],
    [
      newChat, clearChat, openSettings, toggleConsole, clearConsole,
      toggleTerminal, toggleGraph, clearGraph,
      openKnowledge, isStreaming, cancelInference, agentMode, setAgentMode,
      verify, setVerify, yolo, handleYoloChange, openFilePicker, setLeftView,
    ],
  );

  // ---- pane-resize handlers (DRY the shared drag-start + clamp logic) ----
  const SidebarResize = useMemo(
    () => ({
      onDragStart: () => {
        paneStarts.current.sidebar = sidebarW;
      },
      onDelta: (d: number) => {
        setSidebarW(Math.min(520, Math.max(160, paneStarts.current.sidebar + d)));
      },
    }),
    [sidebarW],
  );
  const ChatResize = useMemo(
    () => ({
      onDragStart: () => {
        paneStarts.current.chat = chatW;
      },
      onDelta: (d: number) => {
        setChatW(Math.min(720, Math.max(300, paneStarts.current.chat - d)));
      },
    }),
    [chatW],
  );
  const ConsoleResize = useMemo(
    () => ({
      onDragStart: () => {
        paneStarts.current.console = consoleH;
      },
      onDelta: (d: number) => {
        setConsoleH(Math.min(520, Math.max(96, paneStarts.current.console - d)));
      },
    }),
    [consoleH],
  );

  const openProject = useCallback(
    (path: string) => {
      void applyWorkspace(path);
    },
    [applyWorkspace],
  );

  const closePalette = useCallback(() => setPaletteOpen(false), []);

  const handleToggleSkill = useCallback(
    (name: string, active: boolean) => {
      api
        .skillSetActive(name, active)
        .then(setKnowledge)
        .catch(() => {});
    },
    [setKnowledge],
  );

  return (
    <div className="flex h-full w-full flex-col bg-editor text-ink">
      <TitleBar />
      <MenuBar
        onOpenFolder={selectWorkspace}
        onOpenFile={openFilePicker}
        onSettings={openSettings}
        onSelectModel={loadModel}
        onConsole={toggleConsole}
        consoleVisible={showConsole}
        onTerminal={toggleTerminal}
        terminalVisible={showTerminal}
        onGraph={toggleGraph}
        graphVisible={showGraph}
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
                  ["changes", "Diff"],
                  ["threads", "Threads"],
                  ["resume", "Resume"],
                ] as const
              ).map(([id, label]) => (
              <button
                key={id}
                onClick={() => setLeftView(id)}
                aria-pressed={leftView === id}
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
                  aria-label="New file"
                  className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
                >
                  +
                </button>
                <button
                  onClick={openKnowledge}
                  title="Skills & rules"
                  aria-label="Skills and rules"
                  className="rounded px-1.5 py-0.5 text-sm text-zinc-500 hover:bg-zinc-100 hover:text-zinc-800"
                >
                  ✦
                </button>
                <button
                  onClick={selectWorkspace}
                  title="Open workspace"
                  aria-label="Open workspace"
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
              onOpenSkills={openKnowledge}
              refreshSignal={explorerRefresh}
              onRefresh={refreshExplorer}
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
              onOpenProject={openProject}
            />
          )}
          {leftView === "changes" && (
            <ChangesPanel messages={messages} onDiffResolve={handleDiffResolve} />
          )}
          {leftView === "resume" && (
            <SessionResumePanel
              workspaceRoot={workspaceRoot}
              onResume={switchChat}
              onNewChat={newChat}
            />
          )}
          {leftView === "threads" && (
            <ThreadsPanel
              chatStatus={chatStatus}
              activeSessionId={activeSessionId}
              currentStep={currentStep}
              currentSubtask={currentSubtask}
              runningSubtasks={runningSubtasks}
              modelName={model?.name ?? null}
              ledger={ledger}
              backgroundTasks={backgroundTasks}
              onAbort={abortBackgroundTask}
            />
          )}
        </nav>
        <ResizeHandle
          axis="x"
          onDragStart={SidebarResize.onDragStart}
          onDelta={SidebarResize.onDelta}
        />
        <main className="flex min-w-0 flex-1 flex-col">
          <Tabs
            files={files}
            activeKey={activeKey}
            onSelect={filesSetActive}
            onClose={closeFile}
          />
          <div className="min-h-0 flex-1 overflow-hidden">
            <EditorPane
              file={activeFile}
              onContentChange={updateContent}
              pendingSections={pendingSections}
              hunkResolution={hunkResolution}
              onToggleHunk={handleToggleHunk}
            />
          </div>
        </main>
        <ResizeHandle
          axis="x"
          onDragStart={ChatResize.onDragStart}
          onDelta={ChatResize.onDelta}
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
          customModes={customModes}
          activeCustomMode={activeCustomMode}
          onCustomModeChange={applyCustomMode}
          workflows={workflows}
          onWorkflowInvoke={invokeWorkflow}
          onSend={sendPrompt}
          onCancel={cancelInference}
          onClear={clearChat}
          currentStep={currentStep}
          currentSubtask={currentSubtask}
          queuedCount={queuedCount}
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
          onOpenSkills={openKnowledge}
          onEditResubmit={editResubmit}
          onExport={exportChat}
          exportFormat={exportFormat}
          onExportFormatChange={handleExportFormatChange}
          contextUsage={usage}
          onDiffResolve={handleDiffResolve}
        />
        {isSwitching && (
          <div
            role="status"
            aria-live="polite"
            className="pointer-events-none absolute right-2 top-1/2 z-40 flex -translate-y-1/2 items-center gap-2 rounded-md border border-border bg-panel px-3 py-1.5 text-[11px] text-zinc-500 shadow-sm"
          >
            <svg className="h-3.5 w-3.5 animate-spin text-cyan-600" viewBox="0 0 24 24" fill="none">
              <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" />
              <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8v3a5 5 0 00-5 5H4z" />
            </svg>
            <span>Switching…</span>
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
      {(showConsole || showTerminal || showGraph) && (
        <div className="flex shrink-0 items-center gap-1 border-t border-border bg-panel px-2 pt-1">
          <button
            onClick={toggleConsole}
            className={`rounded px-2 py-0.5 text-[10px] font-semibold ${
              showConsole
                ? "bg-accent/15 text-accent"
                : "text-zinc-500 hover:text-zinc-700"
            }`}
          >
            Console
          </button>
          <button
            onClick={toggleTerminal}
            className={`rounded px-2 py-0.5 text-[10px] font-semibold ${
              showTerminal
                ? "bg-accent/15 text-accent"
                : "text-zinc-500 hover:text-zinc-700"
            }`}
          >
            Terminal
          </button>
          <button
            onClick={toggleGraph}
            className={`rounded px-2 py-0.5 text-[10px] font-semibold ${
              showGraph
                ? "bg-accent/15 text-accent"
                : "text-zinc-500 hover:text-zinc-700"
            }`}
          >
            Execution graph
          </button>
          <div className="flex-1" />
        </div>
      )}
      {showConsole && (
        <ResizeHandle
          axis="y"
          onDragStart={ConsoleResize.onDragStart}
          onDelta={ConsoleResize.onDelta}
        />
      )}
      <ConsolePanel
        entries={consoleEntries}
        visible={showConsole}
        height={consoleH}
        onClear={clearConsole}
      />
      {showTerminal && (
        <ResizeHandle
          axis="y"
          onDragStart={ConsoleResize.onDragStart}
          onDelta={ConsoleResize.onDelta}
        />
      )}
      {showTerminal && (
        <TerminalPanel visible={showTerminal} height={consoleH} cwd={workspaceRoot} />
      )}
      {showGraph && (
        <ResizeHandle
          axis="y"
          onDragStart={ConsoleResize.onDragStart}
          onDelta={ConsoleResize.onDelta}
        />
      )}
      {showGraph && (
        <ExecutionGraphPanel
          state={execGraph}
          visible={showGraph}
          height={consoleH}
          onClear={clearGraph}
        />
      )}
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
      <KnowledgePanel open={showKnowledge} onClose={closeKnowledge} />
      <SettingsModal
        open={showSettings}
        onClose={closeSettings}
        params={genParams}
        onParamsChange={handleParamsChange}
      />
      <CommandPalette
        open={paletteOpen}
        initialMode={paletteMode}
        onClose={closePalette}
        workspaceRoot={workspaceRoot}
        actions={paletteActions}
        skills={paletteSkills}
        modes={customModes}
        activeMode={activeCustomMode}
        onOpenFile={openFile}
        onSwitchChat={switchChat}
        onApplyMode={applyCustomMode}
        onToggleSkill={handleToggleSkill}
      />
    </div>
  );
}
