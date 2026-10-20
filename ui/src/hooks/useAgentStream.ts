import { useCallback, useEffect, useReducer, useRef, useState, type Dispatch, type SetStateAction } from "react";
import { useApp } from "../context/AppContext";
import {
  api,
  isTauriRuntime,
  type EditBatchEvent,
  type PermissionDecision,
  type PermissionRequest,
  type PendingEditGroup,
  type RunTrace,
} from "../lib/ipc";
import {
  IDLE_STAGE,
  reduceAgentStage,
  type AgentStageEvent,
  type AgentStageState,
} from "../lib/agentStages";
import { appendTurn, assistantTurnId } from "../lib/sessionLog";
import {
  emptyExecutionGraph,
  execGraphReducer,
  type ExecutionGraphAction,
} from "../lib/execGraph";
import type { LiveToolState } from "../components/MessageList";

/** A blocking `ask_question` from the agent. */
export interface QuestionRequest {
  requestId: string;
  question: string;
  choices: string[];
}

export interface PlanStepState {
  index: number;
  title: string;
  status: string;
  error?: string;
}

export interface TodoItemState {
  id: number;
  title: string;
  done: boolean;
}

export interface SubtaskState {
  index: number;
  total: number;
  title: string;
  status: string;
}

/** A first-class subagent (`task`) run reported by `agent://subagent-run`. */
export interface SubagentRunState {
  key: number;
  profile: string;
  status: string;
  summary: string;
  startedAt: number;
  durationMs?: number;
  leasedWorkers?: number;
}

export interface BackgroundTaskState {
  id: string;
  label: string;
  status: string;
  detail?: string;
}

/**
 * Turn id of the user message currently being answered, shared across
 * components. This is a module ref rather than hook state on purpose:
 * `useAgentStream` owns the single event subscription, and the composer (a
 * sibling of the chat page) must not open a second one just to record an id.
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

/** Re-dispatch an applied/updated edit batch as a DOM event so panes outside
 *  the React context (e.g. `EditorPane`) can reload the affected file. */
function announceEditApplied(edits: unknown): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new CustomEvent("vencoder-edit-applied", { detail: { edits } }));
}

/** Append one `agent://run-trace` step to the turn's trace list. Steps are
 *  emitted in order and never repeated, so a late/duplicate emit for a step we
 *  already recorded replaces it in place instead of growing the list twice. */
export function appendRunTrace(prev: RunTrace[], step: RunTrace): RunTrace[] {
  if (!step || typeof step.stepIndex !== "number") return prev;
  const at = prev.findIndex((s) => s.stepIndex === step.stepIndex);
  if (at === -1) return [...prev, step];
  if (prev[at] === step) return prev;
  return prev.map((s, i) => (i === at ? step : s));
}

/** Fold one batch event into the pending list; groups are created on first
 *  sight and dropped again once nothing left in them is actionable, so the
 *  review panel only shows batches the user still has a decision on
 *  (`pending` to approve, or `conflict` to resolve via reload/overwrite). */
export function mergeEditBatch(prev: PendingEditGroup[], evt: EditBatchEvent): PendingEditGroup[] {
  if (!evt.groupId) return prev;
  const incoming = new Map(evt.edits.map((e) => [e.id, e]));
  let found = false;
  const next = prev.map((g) => {
    if (g.id !== evt.groupId) return g;
    found = true;
    return { ...g, edits: g.edits.map((e) => incoming.get(e.id) ?? e) };
  });
  if (!found) {
    next.push({ id: evt.groupId, sessionId: evt.sessionId, tool: "edit", edits: evt.edits });
  }
  return next.filter((g) => g.edits.some((e) => e.status === "pending" || e.status === "conflict"));
}

type StreamHandlers = {
  updateLastMessage: ReturnType<typeof useApp>["updateLastMessage"];
  setProcessing: ReturnType<typeof useApp>["setProcessing"];
  addActivity: ReturnType<typeof useApp>["addActivity"];
  setHitlGate: ReturnType<typeof useApp>["setHitlGate"];
  setLiveTools: Dispatch<SetStateAction<Record<string, LiveToolState>>>;
  pushStage: (event: AgentStageEvent) => void;
  dispatchGraph: Dispatch<ExecutionGraphAction>;
};

/**
 * Owns the agent-stream event subscription (tokens, tool events, HITL gates,
 * completion stats) plus live tool-badge state and HITL gate resolution.
 * Shared by ChatPage and the three-panel WorkspacePage so both stay in lock
 * step with a running agent session.
 */
export function useAgentStream() {
  const {
    messages,
    addActivity,
    updateLastMessage,
    setProcessing,
    hitlGate,
    setHitlGate,
    projectPath,
    bumpHistory,
  } = useApp();
  const [liveTools, setLiveTools] = useState<Record<string, LiveToolState>>({});
  const [planSteps, setPlanSteps] = useState<PlanStepState[]>([]);
  const [todos, setTodos] = useState<TodoItemState[]>([]);
  const [subtasks, setSubtasks] = useState<SubtaskState[]>([]);
  const [subagentRuns, setSubagentRuns] = useState<SubagentRunState[]>([]);
  const [bgTasks, setBgTasks] = useState<BackgroundTaskState[]>([]);
  const [permission, setPermission] = useState<PermissionRequest | null>(null);
  const [question, setQuestion] = useState<QuestionRequest | null>(null);
  const [pendingEdits, setPendingEdits] = useState<PendingEditGroup[]>([]);
  const [reviewing, setReviewing] = useState(false);
  const [trimToast, setTrimToast] = useState<{ dropped: number; remaining: number } | null>(null);
  const [verifyToast, setVerifyToast] = useState<{ ok: boolean; command: string; output: string } | null>(null);
  const [stage, setStage] = useState<AgentStageState>(IDLE_STAGE);
  const [execGraph, dispatchExecGraph] = useReducer(execGraphReducer, emptyExecutionGraph);
  const runTraceRef = useRef<RunTrace[]>([]);
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
    pushStage: (event) => {
      setStage((prev) => reduceAgentStage(prev, event, Date.now()));
    },
    dispatchGraph: dispatchExecGraph,
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

  // Detached runs may leave edits awaiting review; a reload must restore the
  // pending batches so nothing is stranded.
  useEffect(() => {
    if (!isTauriRuntime()) return;
    let cancelled = false;
    void api
      .listPendingEdits()
      .then((groups) => {
        if (!cancelled) setPendingEdits(groups ?? []);
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

    api
      .subscribeEngineEvents({
        onToken: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const delta =
            (data as { delta?: string }).delta ?? (data as { content?: string }).content;
          if (delta) h.updateLastMessage({ content: (c) => (c || "") + delta });
          h.pushStage({ type: "token" });
        },
        onStarted: () => {
          const h = handlersRef.current;
          if (!h) return;
          h.setProcessing(true);
          h.setLiveTools({});
          h.dispatchGraph({ type: "reset" });
          h.pushStage({ type: "started" });
          h.addActivity("Agent started", "status");
        },
        onPromptEval: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const tokens = Number((data as { promptTokens?: number }).promptTokens) || 0;
          h.pushStage({ type: "prompt", tokens });
        },
        onDone: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          h.setProcessing(false);
          h.pushStage({ type: "done" });
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
              bumpHistory,
            );
          }
          activeTurnId = "";
          runTraceRef.current = [];
          const stats = [
            done.outputTokens != null ? `${done.outputTokens} tok` : null,
            done.tokensPerSec ? `${done.tokensPerSec.toFixed(1)} tok/s` : null,
            done.elapsedMs ? `${(done.elapsedMs / 1000).toFixed(1)}s` : null,
          ]
            .filter(Boolean)
            .join(" · ");
          h.addActivity(stats ? `Agent finished — ${stats}` : "Agent finished", "status");
        },
        onError: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          h.setProcessing(false);
          h.pushStage({ type: "error" });
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
          h.pushStage({ type: "done" });
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
          const d = data as {
            id?: string;
            tool?: string;
            status?: string;
            summary?: string;
            durationMs?: number;
            exitCode?: number;
          };
          const label = d.tool ? d.tool : "tool";
          h.pushStage({ type: "tool", tool: d.tool, status: d.status });
          if (d.id) {
            const id = d.id;
            const tool = d.tool ?? "tool";
            const status = d.status ?? "running";
            const summary = d.summary ?? "";
            h.setLiveTools((prev) => ({
              ...prev,
              [id]: {
                id,
                tool,
                status,
                summary,
                output: prev[id]?.output,
                durationMs: d.durationMs ?? prev[id]?.durationMs,
                exitCode: d.exitCode ?? prev[id]?.exitCode,
              },
            }));
            const graphStatus =
              status === "done" ? "done" : status === "error" ? "error" : "running";
            h.dispatchGraph({ type: "tool", id, name: tool, status: graphStatus, summary });
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
          h.pushStage({ type: "step" });
          h.dispatchGraph({ type: "step", group: s.group ?? "" });
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
          h.dispatchGraph({
            type: "subtask",
            index,
            total: st.total ?? 0,
            title: st.title ?? `Subtask ${index}`,
            status: st.status ?? "running",
          });
          setSubtasks((prev) => {
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
        onSubagentRun: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            profile?: string;
            status?: string;
            summary?: string;
            startedAt?: number;
            durationMs?: number;
            leasedWorkers?: number;
          };
          const profile = d.profile ?? "subagent";
          const status = d.status ?? "started";
          setSubagentRuns((prev) => {
            const entry: SubagentRunState = {
              key: (d.startedAt ?? Date.now()) ^ prev.length,
              profile,
              status,
              summary: d.summary ?? "",
              startedAt: d.startedAt ?? Date.now(),
              durationMs: d.durationMs,
              leasedWorkers: d.leasedWorkers,
            };
            const next = prev.filter(
              (x) =>
                !(
                  x.profile === profile &&
                  x.status === "started" &&
                  status !== "started"
                ),
            );
            next.push(entry);
            return next;
          });
          const label = `Subagent ${profile} ${status}`;
          h.addActivity(
            d.summary ? `${label} — ${d.summary}` : label,
            status === "error" ? "error" : "status",
          );
        },
        onHook: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const d = data as {
            event?: string;
            tool?: string;
            status?: string;
            detail?: string;
          };
          const label = `Hook ${d.event ?? "hook"}${d.tool ? ` ${d.tool}` : ""}${d.status ? ` ${d.status}` : ""}`;
          h.addActivity(
            d.detail ? `${label} — ${d.detail}` : label,
            d.status === "deny" ? "error" : "status",
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
          h.dispatchGraph({
            type: "planStep",
            planId: d.planId ?? "",
            itemIndex: index,
            title: d.title ?? `Step ${index}`,
            status: d.status ?? "in_progress",
          });
          setPlanSteps((prev) => {
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
          setTodos(items);
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
          setBgTasks((prev) => {
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
          setPermission({
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
          setQuestion({
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
          setTrimToast({ dropped: d.dropped ?? 0, remaining: d.remaining ?? 0 });
        },
        onRunTrace: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const next = appendRunTrace(runTraceRef.current, data as unknown as RunTrace);
          if (next !== runTraceRef.current) {
            runTraceRef.current = next;
            h.updateLastMessage({ trace: next });
          }
        },
        onEditProposed: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const evt = data as unknown as EditBatchEvent;
          if (!evt.groupId) return;
          setPendingEdits((prev) => mergeEditBatch(prev, evt));
          const count = evt.edits.filter((e) => e.status === "pending").length;
          h.addActivity(
            count > 0 ? `Edit proposed — ${count} file${count === 1 ? "" : "s"} awaiting review` : "Edit proposed",
            "status",
          );
        },
        onEditApplied: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const evt = data as unknown as EditBatchEvent;
          if (!evt.groupId) return;
          setPendingEdits((prev) => mergeEditBatch(prev, evt));
          const applied = evt.edits.filter((e) => e.status === "applied").length;
          const rejected = evt.edits.filter((e) => e.status === "rejected").length;
          if (applied > 0) h.addActivity(`Edit${applied > 1 ? "s" : ""} applied — ${applied} file${applied === 1 ? "" : "s"} updated`, "status");
          if (rejected > 0) h.addActivity(`Edit${rejected > 1 ? "s" : ""} rejected`, "status");
        },
        onEditFailed: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const evt = data as unknown as EditBatchEvent;
          if (!evt.groupId) return;
          setPendingEdits((prev) => mergeEditBatch(prev, evt));
          const failed = evt.edits.filter((e) => e.status === "failed" || e.status === "conflict");
          const detail = failed.map((e) => e.error || e.path).filter(Boolean).join("; ");
          h.addActivity(
            detail ? `Edit failed — ${detail}` : "Edit failed",
            "error",
          );
        },
        onSkillsChanged: () => {
          handlersRef.current?.addActivity("Skills changed", "status");
          window.dispatchEvent(new CustomEvent("vencoder-skills-changed"));
        },
        onVerifyResult: (data) => {
          const h = handlersRef.current;
          if (!h) return;
          const evt = data as { ok?: boolean; command?: string; output?: string };
          const label = evt.command ? ` \`${evt.command}\`` : "";
          h.addActivity(evt.ok ? `Verify passed${label}` : `Verify found issues${label}`, evt.ok ? "status" : "error");
          setVerifyToast({ ok: !!evt.ok, command: evt.command ?? "", output: evt.output ?? "" });
        },
        onModelLoaded: () => {},
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

// The gate is driven purely by the backend `agent://hitl-gate` event.
  // A previous text-marker parser also tried to infer the gate from streamed
  // assistant text; its guard keyed `|${reason}` while `resolveGate` recorded
  // `${requestId}|${reason}`, so it never matched and a leftover marker in the
  // transcript could re-open an already-resolved gate with an empty requestId
  // (which then aborted the run and re-issued the prompt). The event carries
  // the requestId, so it is the only source of truth.
  const resolveGate = useCallback(
    async (approved: boolean) => {
      const gate = hitlGate;
      if (!gate?.requestId) return;
      setHitlGate(null);
      await api.hitlRespond(gate.requestId, approved).catch(() => {});
      addActivity(approved ? "Approved - resuming." : "Denied - agent will reroute.", "status");
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

  // Auto-dismiss the context-trim toast after a few seconds.
  useEffect(() => {
    if (!trimToast) return;
    const t = setTimeout(() => setTrimToast(null), 6000);
    return () => clearTimeout(t);
  }, [trimToast]);

  const applyEditGroup = useCallback(
    async (groupId: string, editIds?: string[]) => {
      setReviewing(true);
      try {
        const files = await api.applyEdits(groupId, editIds);
        announceEditApplied(files);
        setPendingEdits((prev) =>
          mergeEditBatch(prev, {
            sessionId: 0,
            groupId,
            edits: files,
            timestampMs: Date.now(),
          }),
        );
      } catch (err) {
        addActivity(
          `Could not apply edits: ${err instanceof Error ? err.message : String(err)}`,
          "error",
        );
        void api.listPendingEdits().then((groups) => setPendingEdits(groups ?? [])).catch(() => {});
      } finally {
        setReviewing(false);
      }
    },
    [addActivity],
  );

  const rejectEditGroup = useCallback(
    async (groupId: string, editIds?: string[], reason?: string) => {
      setReviewing(true);
      try {
        const files = await api.rejectEdits(groupId, editIds, reason);
        announceEditApplied(files);
        setPendingEdits((prev) =>
          mergeEditBatch(prev, {
            sessionId: 0,
            groupId,
            edits: files,
            timestampMs: Date.now(),
          }),
        );
      } catch (err) {
        addActivity(
          `Could not reject edits: ${err instanceof Error ? err.message : String(err)}`,
          "error",
        );
        void api.listPendingEdits().then((groups) => setPendingEdits(groups ?? [])).catch(() => {});
      } finally {
        setReviewing(false);
      }
    },
    [addActivity],
  );

  const resolveConflict = useCallback(
    async (groupId: string, overwrite: boolean, editIds?: string[]) => {
      setReviewing(true);
      try {
        const files = await api.resolveEditConflict(groupId, overwrite, editIds);
        announceEditApplied(files);
        setPendingEdits((prev) =>
          mergeEditBatch(prev, {
            sessionId: 0,
            groupId,
            edits: files,
            timestampMs: Date.now(),
          }),
        );
      } catch (err) {
        addActivity(
          `Could not resolve edit conflict: ${err instanceof Error ? err.message : String(err)}`,
          "error",
        );
        void api.listPendingEdits().then((groups) => setPendingEdits(groups ?? [])).catch(() => {});
      } finally {
        setReviewing(false);
      }
    },
    [addActivity],
  );

  return {
    liveTools,
    stage,
    hitlGate,
    resolveGate,
    planSteps,
    todos,
    subtasks,
    subagentRuns,
    bgTasks,
    execGraph,
    clearExecGraph: () => dispatchExecGraph({ type: "reset" }),
    permission,
    respondPermission,
    question,
    respondQuestion,
    pendingEdits,
    applyEditGroup,
    rejectEditGroup,
    resolveConflict,
    reviewing,
    trimToast,
    dismissTrimToast: () => setTrimToast(null),
    verifyToast,
    dismissVerifyToast: () => setVerifyToast(null),
    runVerify: async (files?: string[]) => {
      try {
        const run = await api.runVerify(files);
        setVerifyToast({ ok: run.ok, command: run.command, output: run.report });
        return run;
      } catch (err) {
        const msg = String((err as { message?: string })?.message ?? err);
        handlersRef.current?.addActivity(`Verify: ${msg}`, "error");
        setVerifyToast({ ok: false, command: "", output: msg });
        return null;
      }
    },
    clearRunState: () => {
      setPlanSteps([]);
      setTodos([]);
      setSubtasks([]);
      setSubagentRuns([]);
      dispatchExecGraph({ type: "reset" });
    },
    abortBackgroundTask: async (taskId: string) => {
      await api.abortBackgroundTask(taskId).catch(() => {});
      setBgTasks((prev) => prev.filter((x) => x.id !== taskId));
    },
  };
}
