import React, {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type Dispatch,
  type SetStateAction,
} from "react";
import { useApp } from "./AppContext";
import { api, isTauriRuntime, type ContextUsage, type PermissionDecision, type PermissionRequest } from "../lib/ipc";
import { parseChatSegments } from "../lib/chatParse";
import { appendTurn, assistantTurnId } from "../lib/sessionLog";
import type {
  BackgroundTaskState,
  LiveToolState,
  PlanStepState,
  QuestionRequest,
  SubtaskState,
  TodoItemState,
} from "./types";

/**
 * Turn id of the user message currently being answered, shared across
 * components. This is a module ref rather than context state on purpose: the
 * composer must be able to record an id without opening a second event
 * subscription, and the single subscriber below is the only reader.
 */
let activeTurnId = "";

/**
 * Chat the active turn belongs to. `session_append` writes to a per-chat file,
 * so an assistant reply has to target the same chat the user turn went to.
 */
let activeChatId: string | null = null;

export function setActiveTurnId(turnId: string): void {
  activeTurnId = turnId;
}

export function setActiveChatId(chatId: string | null): void {
  activeChatId = chatId;
}

export interface AgentStreamValue {
  liveTools: Record<string, LiveToolState>;
  hitlGate: { requestId: string; reason: string } | null;
  resolveGate: (approved: boolean) => Promise<void>;
  planSteps: PlanStepState[];
  todos: TodoItemState[];
  subtasks: SubtaskState[];
  bgTasks: BackgroundTaskState[];
  abortBackgroundTask: (taskId: string) => Promise<void>;
  permission: PermissionRequest | null;
  respondPermission: (decision: PermissionDecision) => Promise<void>;
  question: QuestionRequest | null;
  respondQuestion: (answer: string) => Promise<void>;
  clearRunState: () => void;
  /** Latest `context_status` reading; null until the first successful probe. */
  contextUsage: ContextUsage | null;
  refreshContextUsage: () => void;
}

const AgentStreamContext = createContext<AgentStreamValue | null>(null);

export function useAgentStream(): AgentStreamValue {
  const ctx = useContext(AgentStreamContext);
  if (!ctx) throw new Error("useAgentStream must be used within AgentStreamProvider");
  return ctx;
}

type StreamHandlers = {
  updateLastMessage: ReturnType<typeof useApp>["updateLastMessage"];
  setProcessing: ReturnType<typeof useApp>["setProcessing"];
  addActivity: ReturnType<typeof useApp>["addActivity"];
  setHitlGate: ReturnType<typeof useApp>["setHitlGate"];
  setLiveTools: Dispatch<SetStateAction<Record<string, LiveToolState>>>;
  setPlanSteps: Dispatch<SetStateAction<PlanStepState[]>>;
  setTodos: Dispatch<SetStateAction<TodoItemState[]>>;
  setSubtasks: Dispatch<SetStateAction<SubtaskState[]>>;
  setBgTasks: Dispatch<SetStateAction<BackgroundTaskState[]>>;
  setPermission: Dispatch<SetStateAction<PermissionRequest | null>>;
  setQuestion: Dispatch<SetStateAction<QuestionRequest | null>>;
  setContextUsage: Dispatch<SetStateAction<ContextUsage | null>>;
  clearResolvedKey: () => void;
};

/**
 * Owns the one and only `subscribeEngineEvents` subscription for the agent
 * stream (tokens, tool events, HITL gates, completion stats) plus the live
 * tool badges, run bookkeeping and HITL resolution. It is mounted above the
 * router, so leaving `/chat` for `/workspace` mid-run cannot drop the
 * subscription or reset the run state.
 */
export function AgentStreamProvider({ children }: { children: React.ReactNode }) {
  const {
    messages,
    addActivity,
    updateLastMessage,
    setProcessing,
    isProcessing,
    hitlGate,
    setHitlGate,
    projectPath,
  } = useApp();
  const [liveTools, setLiveTools] = useState<Record<string, LiveToolState>>({});
  const [planSteps, setPlanSteps] = useState<PlanStepState[]>([]);
  const [todos, setTodos] = useState<TodoItemState[]>([]);
  const [subtasks, setSubtasks] = useState<SubtaskState[]>([]);
  const [bgTasks, setBgTasks] = useState<BackgroundTaskState[]>([]);
  const [permission, setPermission] = useState<PermissionRequest | null>(null);
  const [question, setQuestion] = useState<QuestionRequest | null>(null);
  const [contextUsage, setContextUsage] = useState<ContextUsage | null>(null);
  const resolvedKeyRef = useRef<string>("");
  const messagesRef = useRef(messages);
  messagesRef.current = messages;
  const projectRef = useRef(projectPath);
  projectRef.current = projectPath;

  const handlersRef = useRef<StreamHandlers | null>(null);
  handlersRef.current = {
    updateLastMessage,
    setProcessing,
    addActivity,
    setHitlGate,
    setLiveTools,
    setPlanSteps,
    setTodos,
    setSubtasks,
    setBgTasks,
    setPermission,
    setQuestion,
    setContextUsage,
    clearResolvedKey: () => {
      resolvedKeyRef.current = "";
    },
  };

  // Detached runs outlive the page, so seed the timeline from the backend
  // registry: a reload must not lose sight of work that is still going.
  useEffect(() => {
    if (!isTauriRuntime()) return;
    let cancelled = false;
    void api
      .listBackgroundTasks()
      .then((list) => {
        if (cancelled) return;
        setBgTasks(
          (list ?? []).map((t) => ({
            id: t.id,
            label: t.label,
            status: t.status,
          })),
        );
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;

    const refreshUsage = () => {
      void api
        .contextStatus()
        .then((usage) => {
          if (!disposed) setContextUsage(usage ?? null);
        })
        .catch(() => {});
    };

    api
      .subscribeEngineEvents({
        onToken: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const delta =
            (data as { delta?: string }).delta ?? (data as { content?: string }).content;
          if (delta) h.updateLastMessage({ content: (c) => (c || "") + delta });
        },
        onStarted: () => {
          const h = handlersRef.current;
          if (!h) return;
          h.setProcessing(true);
          h.setLiveTools({});
          h.clearResolvedKey();
          h.addActivity("Agent started", "status");
          refreshUsage();
        },
        onDone: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          h.setProcessing(false);
          const d = data as {
            done?: {
              outcome?: string;
              outputTokens?: number;
              inputTokens?: number;
              tokensPerSec?: number;
              elapsedMs?: number;
              stopReason?: string;
            };
          };
          const done = d.done || {};
          h.updateLastMessage({ streaming: false, done });
          const last = messagesRef.current[messagesRef.current.length - 1];
          if (last?.role === "assistant" && (last.content || "").trim()) {
            void api.contextPushTurn("assistant", last.content).catch(() => {});
            void appendTurn(
              projectRef.current,
              {
                role: "assistant",
                content: last.content,
                ts: Date.now(),
                done: done as Record<string, unknown>,
                // Distinct from the user's turn id: `session_append` dedupes by
                // turnId, so reusing it would drop the reply as a replay.
                turnId: activeTurnId ? assistantTurnId(activeTurnId) : undefined,
              },
              activeChatId,
            );
          }
          activeTurnId = "";
          const stats = [
            done.outputTokens != null ? `${done.outputTokens} tok` : null,
            done.tokensPerSec ? `${done.tokensPerSec.toFixed(1)} tok/s` : null,
            done.elapsedMs ? `${(done.elapsedMs / 1000).toFixed(1)}s` : null,
          ]
            .filter(Boolean)
            .join(" · ");
          h.addActivity(stats ? `Agent finished — ${stats}` : "Agent finished", "status");
          refreshUsage();
        },
        onError: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          h.setProcessing(false);
          const msg = (data as { message?: string }).message || "Agent error";
          h.addActivity(msg, "error");
          h.updateLastMessage({
            streaming: false,
            content: (c) => `${c ? `${c}\n` : ""}\`\`\`error\n${msg}\n\`\`\``,
          });
        },
        onAborted: () => {
          const h = handlersRef.current;
          if (!h) return;
          h.setProcessing(false);
          h.updateLastMessage({ streaming: false });
          h.addActivity("Execution aborted", "status");
        },
        onToolOutput: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as { id?: string; output?: string };
          if (d.id) {
            const id = d.id;
            const output = d.output ?? "";
            h.setLiveTools((prev) =>
              prev[id] ? { ...prev, [id]: { ...prev[id], output } } : prev,
            );
          }
        },
        onTool: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as { id?: string; tool?: string; status?: string; summary?: string };
          const label = d.tool ? d.tool : "tool";
          if (d.id) {
            const id = d.id;
            const tool = d.tool ?? "tool";
            const status = d.status ?? "running";
            const summary = d.summary ?? "";
            h.setLiveTools((prev) => ({
              ...prev,
              [id]: { id, tool, status, summary, output: prev[id]?.output },
            }));
          }
          if (d.status === "running") {
            h.addActivity(`${label} starting…`, "tool");
          } else if (d.status === "done") {
            h.addActivity(`${label} complete`, "tool");
          } else if (d.status === "error") {
            h.addActivity(`${label} failed: ${d.summary || ""}`, "error");
          }
        },
        onFileChanged: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as { path?: string };
          if (d.path) h.addActivity(`File changed: ${d.path}`, "status");
        },
        onHitlGate: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as { requestId?: string; reason?: string };
          if (d.requestId) {
            const reason = d.reason || "Approval required for the next action";
            h.setHitlGate((prev) =>
              prev && prev.requestId === d.requestId
                ? prev
                : { requestId: d.requestId as string, reason },
            );
            h.addActivity("Approval requested — run paused", "status");
          }
        },
        onStep: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            step?: { step?: number; group?: string; tokens?: number; elapsedMs?: number; toolCalls?: number };
          };
          const s = d.step;
          if (!s) return;
          const label = s.group ? `${s.group} — step ${s.step ?? "?"}` : `Step ${s.step ?? "?"}`;
          const bits = [
            s.toolCalls ? `${s.toolCalls} tool call${s.toolCalls === 1 ? "" : "s"}` : null,
            s.tokens ? `${s.tokens} tok` : null,
            s.elapsedMs ? `${(s.elapsedMs / 1000).toFixed(1)}s` : null,
          ]
            .filter(Boolean)
            .join(" · ");
          h.addActivity(bits ? `${label} (${bits})` : label, "status");
        },
        onSubtask: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            subtask?: { index?: number; total?: number; title?: string; status?: string; elapsedMs?: number };
          };
          const st = d.subtask;
          if (!st) return;
          const index = st.index ?? 0;
          const entry: SubtaskState = {
            index,
            total: st.total ?? 0,
            title: st.title ?? `Subtask ${index}`,
            status: st.status ?? "running",
          };
          h.setSubtasks((prev) => {
            const next = prev.filter((x) => x.index !== index);
            next.push(entry);
            next.sort((a, b) => a.index - b.index);
            return next;
          });
          h.addActivity(
            `Subtask ${index}/${st.total ?? "?"} ${st.status ?? ""} — ${st.title ?? ""}`.trim(),
            st.status === "failed" ? "error" : "status",
          );
        },
        onPlanStep: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            planId?: string;
            itemIndex?: number;
            title?: string;
            status?: string;
            error?: string | null;
          };
          const index = d.itemIndex ?? 0;
          if (!index) return;
          h.setPlanSteps((prev) => {
            const next = prev.filter((x) => x.index !== index);
            next.push({
              index,
              title: d.title ?? `Step ${index}`,
              status: d.status ?? "in_progress",
              error: d.error ?? undefined,
            });
            next.sort((a, b) => a.index - b.index);
            return next;
          });
          h.addActivity(
            d.error
              ? `Plan step ${index} failed: ${d.error}`
              : `Plan step ${index}/${index} — ${d.title ?? ""} ${d.status ?? ""}`.trim(),
            d.error ? "error" : "status",
          );
        },
        onTodoUpdate: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as { items?: Array<{ id?: number; title?: string; done?: boolean }> };
          const items = (d.items ?? []).map((it, i) => ({
            id: it.id ?? i + 1,
            title: it.title ?? "",
            done: Boolean(it.done),
          }));
          h.setTodos(items);
          const doneCount = items.filter((it) => it.done).length;
          h.addActivity(`Todos ${doneCount}/${items.length}`, "status");
        },
        onBgTask: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            taskId?: string;
            label?: string;
            status?: string;
            detail?: string | null;
          };
          const id = d.taskId;
          if (!id) return;
          // The backend emits "started" on spawn, never "running".
          const raw = d.status ?? "started";
          const status = raw === "started" ? "running" : raw;
          h.setBgTasks((prev) => {
            const next = prev.filter((x) => x.id !== id);
            // Terminal states are kept so the outcome stays visible; the
            // timeline only offers abort for the still-running ones.
            next.push({ id, label: d.label ?? id, status, detail: d.detail ?? undefined });
            return next;
          });
          h.addActivity(
            `Background ${status}: ${d.label ?? id}${d.detail ? ` — ${d.detail}` : ""}`,
            status === "error" ? "error" : "status",
          );
        },
        onPermission: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            requestId?: string;
            tool?: string;
            summary?: string;
            review?: string;
          };
          if (!d.requestId) return;
          h.setPermission({
            requestId: d.requestId,
            tool: d.tool ?? "tool",
            summary: d.summary ?? "",
            timestampMs: Date.now(),
            review: d.review,
          });
          h.addActivity(`Approval requested for ${d.tool ?? "a tool"} — run paused`, "status");
        },
        onQuestion: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            requestId?: string;
            question?: string;
            choices?: string[];
          };
          if (!d.requestId) return;
          h.setQuestion({
            requestId: d.requestId,
            question: d.question ?? "",
            choices: d.choices ?? [],
          });
        },
        onContextTrimmed: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as { dropped?: number; remaining?: number };
          h.addActivity(
            `Context trimmed — dropped ${d.dropped ?? 0}, ${d.remaining ?? 0} turns kept`,
            "status",
          );
          refreshUsage();
        },
        onSkillsChanged: () => {
          handlersRef.current?.addActivity("Skills changed — reloaded", "status");
        },
        onModelLoaded: () => {
          handlersRef.current?.addActivity("Model loaded", "status");
        },
        onLoadProgress: () => {},
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
  }, []);

  useEffect(() => {
    if (!isProcessing || messages.length === 0) return;
    const last = messages[messages.length - 1];
    if (last.role !== "assistant" || !last.streaming) return;
    const gate = parseChatSegments(last.content).find((s) => s.kind === "gate");
    if (gate) {
      const reason = (gate as { reason: string }).reason;
      const key = `|${reason}`;
      if (resolvedKeyRef.current === key) return;
      if (!hitlGate || hitlGate.reason !== reason) {
        setHitlGate((prev) => (!prev ? { requestId: "", reason } : prev));
      }
    } else if (hitlGate) {
      setHitlGate(null);
    }
  }, [messages, isProcessing, setHitlGate, hitlGate]);

  const resolveGate = useCallback(
    async (approved: boolean) => {
      const gate = hitlGate;
      if (!gate) return;
      setHitlGate(null);
      resolvedKeyRef.current = `${gate.requestId}|${gate.reason}`;
      if (gate.requestId) {
        await api.hitlRespond(gate.requestId, approved).catch(() => {});
        addActivity(approved ? "Approved — resuming…" : "Denied — agent will reroute…", "status");
        return;
      }
      void api.abortAgentExecution().catch(() => {});
      const text = approved
        ? `[HITL Approved] ${gate.reason}`
        : `[HITL Denied] ${gate.reason} — do not perform this action. Propose an alternative or reroute.`;
      window.dispatchEvent(new CustomEvent("vencoder-send-prompt", { detail: text }));
    },
    [hitlGate, setHitlGate, addActivity],
  );

  const respondPermission = useCallback(
    async (decision: PermissionDecision) => {
      const req = permission;
      if (!req) return;
      setPermission(null);
      await api.agentRespondPermission(req.requestId, decision).catch(() => {});
      addActivity(
        decision === "deny" ? "Denied — agent will reroute…" : `Approved (${decision})`,
        "status",
      );
    },
    [permission, addActivity],
  );

  const respondQuestion = useCallback(
    async (answer: string) => {
      const req = question;
      if (!req) return;
      setQuestion(null);
      await api.agentRespondQuestion(req.requestId, answer).catch(() => {});
    },
    [question],
  );

  const clearRunState = useCallback(() => {
    setPlanSteps([]);
    setTodos([]);
    setSubtasks([]);
  }, []);

  const abortBackgroundTask = useCallback(async (taskId: string) => {
    await api.abortBackgroundTask(taskId).catch(() => {});
    setBgTasks((prev) => prev.filter((x) => x.id !== taskId));
  }, []);

  const refreshContextUsage = useCallback(() => {
    if (!isTauriRuntime()) return;
    void api
      .contextStatus()
      .then((usage) => setContextUsage(usage ?? null))
      .catch(() => {});
  }, []);

  const value: AgentStreamValue = useMemo(
    () => ({
      liveTools,
      hitlGate,
      resolveGate,
      planSteps,
      todos,
      subtasks,
      bgTasks,
      abortBackgroundTask,
      permission,
      respondPermission,
      question,
      respondQuestion,
      clearRunState,
      contextUsage,
      refreshContextUsage,
    }),
    [
      liveTools,
      hitlGate,
      resolveGate,
      planSteps,
      todos,
      subtasks,
      bgTasks,
      abortBackgroundTask,
      permission,
      respondPermission,
      question,
      respondQuestion,
      clearRunState,
      contextUsage,
      refreshContextUsage,
    ],
  );

  return <AgentStreamContext.Provider value={value}>{children}</AgentStreamContext.Provider>;
}
