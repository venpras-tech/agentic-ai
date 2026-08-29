import { useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";

import type {
  AgentToolEvent,
  ChatMessage,
  ContextUsage,
  InferenceDone,
  QuestionRequest,
  StepTimelineStep,
  TodoUpdateEvent,
} from "../types";
import DiffView from "./DiffView";
import MarkdownRenderer from "./MarkdownRenderer";
import StatusIndicator from "./StatusIndicator";
import { api } from "../lib/ipc";
import type { ChatStatus } from "../lib/chatStatus";

export interface SendOptions {
  planMode?: boolean;
  verify?: boolean;
  decompose?: boolean;
}

interface ChatPanelProps {
  messages: ChatMessage[];
  /** Draggable pane width in px; falls back to the old fixed w-96. */
  width?: number;
  streams: ReadonlyMap<number, string>;
  activeSessionId: number | null;
  isStreaming: boolean;
  /** Animated turn-lifecycle state (see lib/chatStatus.ts). */
  status: ChatStatus;
  lastDone: InferenceDone | null;
  modelName: string | null;
  agentMode: boolean;
  onAgentModeChange: (v: boolean) => void;
  onSend: (text: string, opts?: SendOptions) => void;
  onCancel: () => void;
  onClear: () => void;
  currentStep: number | null;
  currentSubtask: { index: number; total: number; title: string } | null;
  verify: boolean;
  onVerifyChange: (v: boolean) => void;
  /** YOLO sub-mode (Bionic §3.3): ROUTINE shell commands skip approval. */
  yolo?: boolean;
  onYoloChange?: (v: boolean) => void;
  /** Available skill names for @-mention autocomplete. */
  skills?: string[];
  /** Files attached to this session's RAG index. */
  attachments?: { path: string; chunkCount: number }[];
  onAttachClick?: () => void;
  onDetachFile?: (path: string) => void;
  pendingPlan: { sessionId: number; planText: string } | null;
  onApprovePlan: () => void;
  onRejectPlan: () => void;
  onOpenSkills: () => void;
  todos: TodoUpdateEvent | null;
  /** Blocking `ask_question` request rendered inline above the composer. */
  questionReq: QuestionRequest | null;
  onRespondQuestion: (requestId: string, answer: string) => void;
  /** Edit a previous user message: truncates history and resubmits. */
  onEditResubmit?: (newText: string, messageIndex: number) => void;
  /** Export the conversation. */
  onExport?: () => void;
  /** Current export format (pdf/docx/csv). */
  exportFormat?: string;
  /** Change the export format. */
  onExportFormatChange?: (fmt: string) => void;
  /** Context window usage for the pressure indicator. */
  contextUsage?: ContextUsage | null;
  /** Called when files are dropped onto the chat panel. */
  onDropFiles?: (paths: string[]) => void;
}

const SLASH_HINTS: { cmd: string; hint: string }[] = [
  { cmd: "/plan", hint: "draft a plan, then approve to execute" },
  { cmd: "/act", hint: "execute a task with tools" },
  { cmd: "/decompose", hint: "break a large task into subtasks, then execute" },
  { cmd: "/fix", hint: "diagnose, fix and verify" },
  { cmd: "/bug", hint: "analyze a bug descriptor and propose a fix" },
  { cmd: "/review", hint: "review a file or diff for issues" },
  { cmd: "/test", hint: "run tests and fix failures" },
  { cmd: "/commit", hint: "checkpoint git commit" },
  { cmd: "/skills", hint: "open knowledge panel" },
  { cmd: "/clear", hint: "clear conversation" },
];

const MAX_INPUT_CHARS = 1_000_000;

/** Human-readable label + badge style for a turn outcome. */
function outcomeLabel(outcome: string): string {
  switch (outcome) {
    case "completed":
      return "done";
    case "failed":
      return "failed";
    case "interrupted":
      return "interrupted";
    case "error":
      return "error";
    default:
      return outcome;
  }
}

/** Small color-coded lifecycle badge shown on a finished assistant turn. */
function OutcomeBadge({ outcome }: { outcome: string }) {
  const styles: Record<string, string> = {
    completed: "bg-emerald-500/15 text-emerald-600",
    failed: "bg-red-500/15 text-red-600",
    interrupted: "bg-amber-500/15 text-amber-600",
    error: "bg-red-500/15 text-red-600",
  };
  return (
    <span
      className={`rounded px-1.5 py-px text-[9px] font-semibold normal-case tracking-normal ${
        styles[outcome] ?? "bg-zinc-500/15 text-zinc-500"
      }`}
    >
      {outcomeLabel(outcome)}
    </span>
  );
}

/** Grouped, collapsible per-step telemetry for one assistant turn. Steps
 *  arriving from the orchestrator carry a phase label ("Plan", "Execute" or
 *  "Subtask N/M · title"); we group by that label, preserving first-seen order,
 *  and collapse everything into a single header when the turn has one group. */
function StepTimeline({ steps }: { steps: StepTimelineStep[] }) {
  const groups: { label: string; steps: StepTimelineStep[] }[] = [];
  const seen = new Map<string, number>();
  for (const s of steps) {
    const idx = seen.get(s.group);
    if (idx == null) {
      seen.set(s.group, groups.length);
      groups.push({ label: s.group, steps: [s] });
    } else {
      groups[idx].steps.push(s);
    }
  }

  const [collapsed, setCollapsed] = useState<Set<string>>(() =>
    groups.length > 1 ? new Set(groups.slice(1).map((g) => g.label)) : new Set(),
  );
  const toggle = (label: string) =>
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(label)) next.delete(label);
      else next.add(label);
      return next;
    });

  if (groups.length === 0) return null;
  return (
    <div className="mt-2 space-y-1 rounded-md border border-border bg-panel-2/40 p-1.5">
      {groups.map((g) => {
        const isCollapsed = collapsed.has(g.label);
        const tokens = g.steps.reduce((a, s) => a + s.tokens, 0);
        const tools = g.steps.reduce((a, s) => a + s.toolCalls, 0);
        return (
          <div key={g.label}>
            <button
              onClick={() => toggle(g.label)}
              className="flex w-full items-center gap-1.5 rounded px-1.5 py-0.5 text-left text-[10.5px] hover:bg-panel-2/70"
            >
              <span className="text-zinc-400">{isCollapsed ? "▸" : "▾"}</span>
              <span className="truncate font-medium text-zinc-700">{g.label}</span>
              <span className="ml-auto shrink-0 text-zinc-400">
                {g.steps.length} step{g.steps.length !== 1 ? "s" : ""} · {tokens} tok
                {tools > 0 ? ` · ${tools} tool${tools !== 1 ? "s" : ""}` : ""}
              </span>
            </button>
            {!isCollapsed && (
              <div className="ml-3 space-y-0.5 border-l border-border pl-2">
                {g.steps.map((s, i) => (
                  <div key={i} className="flex items-center gap-2 text-[10px] text-zinc-500">
                    <span className="text-zinc-500">#{s.step}</span>
                    <span>{s.tokens} tok</span>
                    <span>{s.elapsedMs}ms</span>
                    {s.toolCalls > 0 && <span>{s.toolCalls} tool(s)</span>}
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

function ToolCard({ event }: { event: AgentToolEvent }) {
  const running = event.status === "running";
  const failed = event.status === "error";
  const hasBody =
    (event.output?.trim().length ?? 0) > 0 || Boolean(event.detail);
  // While a call is running the card stays open (live output streams in);
  // finished calls collapse to a single line. A manual toggle overrides both.
  const [override, setOverride] = useState<boolean | null>(null);
  const open = override ?? running;

  return (
    <div className="rounded-md border border-border bg-panel-2/70">
      <button
        onClick={() => setOverride(!open)}
        title={open ? "Collapse" : "Expand"}
        className="flex w-full items-center gap-1.5 px-2 py-1.5 text-left text-[10.5px]"
      >
        <span
          className={`h-1.5 w-1.5 shrink-0 rounded-full ${
            running ? "animate-pulse bg-amber-400" : failed ? "bg-red-400" : "bg-emerald-400"
          }`}
        />
        <span className="shrink-0 font-medium text-zinc-700">
          {event.tool.replaceAll("_", " ")}
        </span>
        <span className={`truncate ${running ? "text-amber-700/80" : "text-zinc-500"}`}>
          {event.summary}
        </span>
        <span className="ml-auto flex shrink-0 items-center gap-1 text-zinc-400">
          {running ? (
            <span className="animate-pulse">running…</span>
          ) : event.durationMs != null ? (
            `${event.durationMs}ms`
          ) : (
            event.status
          )}
          <span className="text-zinc-300">{open ? "▾" : "▸"}</span>
        </span>
      </button>
      {open && (
        <div className="px-2 pb-1.5">
          {!hasBody && (
            <p className="text-[10.5px] leading-snug text-zinc-500">{event.summary}</p>
          )}
          {(event.output?.trim().length ?? 0) > 0 && (
            <pre className="mt-0.5 max-h-40 overflow-auto whitespace-pre-wrap rounded bg-zinc-100 px-1.5 py-1 font-mono text-[10px] leading-snug text-zinc-500">
              {event.output}
            </pre>
          )}
          {event.detail && (
            <p className="mt-0.5 max-h-24 overflow-auto whitespace-pre-wrap text-[10px] leading-snug text-red-600/80">
              {event.detail}
            </p>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Inline agentic activity feed
// ---------------------------------------------------------------------------

/** One entry of a turn's chronological activity feed. */
type TurnSegment =
  | { kind: "text"; text: string }
  | { kind: "tool"; event: AgentToolEvent };

/**
 * Split a turn's text into segments interleaved with its tool calls using
 * each call's `atChar` anchor — the character offset into the streamed text
 * at the moment the call fired. This makes agent activity appear INLINE
 * between paragraphs exactly where it happened, instead of stacking every
 * card under the finished text. Calls without an anchor (legacy turns) are
 * returned separately and rendered after the text like before.
 */
export function buildTurnSegments(
  message: Pick<ChatMessage, "content" | "tools">,
  liveText: string,
): { segments: TurnSegment[]; unanchored: AgentToolEvent[] } {
  const all = [...(message.tools ?? [])].sort((a, b) => a.startedAt - b.startedAt);
  const anchored = all.filter((t) => t.atChar != null);
  const unanchored = all.filter((t) => t.atChar == null);

  const full = message.content.length > 0 ? message.content : liveText;
  const segments: TurnSegment[] = [];
  let cursor = 0;
  for (const t of anchored) {
    const at = Math.min(t.atChar ?? 0, full.length);
    if (at > cursor) {
      segments.push({ kind: "text", text: full.slice(cursor, at) });
      cursor = at;
    } else if (at < cursor) {
      // Out-of-order anchor (shouldn't happen) — keep chronology sane.
      segments.push({ kind: "tool", event: t });
      continue;
    }
    segments.push({ kind: "tool", event: t });
  }
  const tail = full.slice(cursor);
  if (tail.length > 0 || segments.length === 0) {
    segments.push({ kind: "text", text: tail });
  }
  return { segments, unanchored };
}

/** Live todo checklist card, updated whenever the agent calls set_todo_list
 *  or mark_todo_item_done. Shows open/total progress and per-item state. */
function TodoCard({ todos }: { todos: TodoUpdateEvent }) {
  const done = todos.items.filter((t) => t.done).length;
  const total = todos.items.length;
  return (
    <div className="rounded-md border border-border bg-panel-2/70 px-2 py-1.5">
      <div className="flex items-center gap-1.5 text-[10.5px]">
        <span className="font-medium text-zinc-700">Todo list</span>
        <span className="ml-auto shrink-0 text-zinc-400">
          {done}/{total} done
        </span>
      </div>
      <ul className="mt-1 space-y-0.5">
        {todos.items.map((t) => (
          <li key={t.id} className="flex items-start gap-1.5 text-[11px] leading-snug">
            <span className={`shrink-0 ${t.done ? "text-emerald-500" : "text-zinc-400"}`}>
              {t.done ? "☑" : "☐"}
            </span>
            <span className={t.done ? "text-zinc-400 line-through" : "text-zinc-700"}>
              {t.title}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Inline blocking question from the agent (`ask_question`). Rendered in the
 *  conversation flow above the composer — the agent waits for the answer, so
 *  it stays visible without covering the transcript like a modal. */
function QuestionCard({
  request,
  onRespond,
}: {
  request: QuestionRequest;
  onRespond: (requestId: string, answer: string) => void;
}) {
  const [draft, setDraft] = useState("");
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    setDraft("");
    inputRef.current?.focus();
  }, [request.requestId]);

  const submit = () => {
    const text = draft.trim();
    if (!text) return;
    onRespond(request.requestId, text);
  };

  return (
    <div className="shrink-0 border-t border-cyan-500/30 bg-cyan-500/5 px-3 py-2">
      <div className="mb-1 flex items-center gap-1.5 text-[10.5px]">
        <span className="flex h-4 w-4 items-center justify-center rounded-full bg-cyan-500/20 text-[9px] font-bold text-cyan-600">
          ?
        </span>
        <span className="font-semibold uppercase tracking-wider text-zinc-400">
          The agent has a question
        </span>
        <button
          onClick={() => onRespond(request.requestId, "[no answer]")}
          className="ml-auto shrink-0 rounded border border-border px-1.5 py-px text-[10px] font-medium text-zinc-400 hover:text-zinc-600"
          title="Skip — the agent proceeds with its best judgment"
        >
          Skip
        </button>
      </div>
      <p className="whitespace-pre-wrap text-[12px] leading-relaxed text-ink">
        {request.question}
      </p>
      {request.choices.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {request.choices.map((choice) => (
            <button
              key={choice}
              onClick={() => onRespond(request.requestId, choice)}
              title={choice}
              className="max-w-full truncate rounded border border-emerald-500/30 bg-emerald-500/10 px-2 py-1 text-[11px] font-medium text-emerald-600 hover:bg-emerald-500/20"
            >
              {choice}
            </button>
          ))}
        </div>
      )}
      <div className="mt-1.5 flex gap-1.5">
        <input
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder="Or type your own answer…"
          className="min-w-0 flex-1 rounded border border-border bg-panel px-2 py-1 text-[12px] text-ink outline-none focus:border-accent"
        />
        <button
          onClick={submit}
          disabled={!draft.trim()}
          className="shrink-0 rounded bg-accent px-2.5 py-1 text-[11px] font-semibold text-white hover:bg-cyan-500 disabled:cursor-not-allowed disabled:opacity-40"
        >
          Answer
        </button>
      </div>
    </div>
  );
}

/** One assistant (or error) turn rendered as a chronological activity feed:
 *  streamed text interleaved with inline tool cards at their `atChar`
 *  anchors, then plan controls, legacy unanchored cards, step timeline and
 *  file diffs. While `live`, the trailing text carries a pulsing caret. */
function AssistantTurn({
  message,
  live,
  liveText,
  pendingPlan,
  onApprovePlan,
  onRejectPlan,
}: {
  message: ChatMessage;
  live: boolean;
  liveText: string;
  pendingPlan: { sessionId: number; planText: string } | null;
  onApprovePlan: () => void;
  onRejectPlan: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const copyTimer = useRef<number | null>(null);
  const [bulkResolved, setBulkResolved] = useState<"accepted" | "rejected" | null>(null);
  const isError = message.role === "error";
  const { segments, unanchored } = useMemo(
    () =>
      isError
        ? { segments: [{ kind: "text", text: message.content } as TurnSegment], unanchored: [] }
        : buildTurnSegments(message, liveText),
    [isError, message, liveText],
  );
  const lastTextIdx = segments.reduce(
    (acc, s, i) => (s.kind === "text" ? i : acc),
    -1,
  );
  // Everything the turn currently shows as prose — what Copy should capture
  // (final content when done, the in-flight text while still streaming).
  const turnText = useMemo(
    () =>
      segments
        .filter((s) => s.kind === "text")
        .map((s) => s.text)
        .join(""),
    [segments],
  );

  useEffect(
    () => () => {
      if (copyTimer.current != null) window.clearTimeout(copyTimer.current);
    },
    [],
  );

  const onCopy = async () => {
    try {
      await navigator.clipboard.writeText(turnText);
      setCopied(true);
      if (copyTimer.current != null) window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard denied/unavailable — nothing sensible to do
    }
  };

  return (
    <div className="mb-3">
      <div className="mb-0.5 flex items-center gap-2 text-[10px] uppercase tracking-wider text-zinc-400">
        <span>{isError ? "error" : "assistant"}</span>
        {message.done && !isError && <OutcomeBadge outcome={message.done.outcome} />}
        {(turnText.trim().length > 0 || live) && (
          <div className="ml-auto flex shrink-0 gap-1">
            <button
              onClick={onCopy}
              title="Copy response"
              className="rounded border border-border px-1.5 py-px text-[9px] normal-case tracking-normal text-zinc-400 hover:text-zinc-600"
            >
              {copied ? "copied ✓" : "copy"}
            </button>
          </div>
        )}
      </div>
      <div className="space-y-1.5">
        {segments.map((seg, i) =>
          seg.kind === "text" ? (
            <div
              key={`t-${i}`}
              className={`leading-relaxed ${
                isError ? "whitespace-pre-wrap text-red-600" : ""
              }`}
            >
              {isError ? (
                seg.text
              ) : (
                <MarkdownRenderer content={seg.text} />
              )}
              {live && i === lastTextIdx && (
                <span className="animate-pulse text-accent">▌</span>
              )}
            </div>
          ) : (
            <ToolCard key={seg.event.id} event={seg.event} />
          ),
        )}
      </div>
      {!isError && pendingPlan && (
        <div className="mt-2 flex items-center gap-2">
          <button
            onClick={onApprovePlan}
            className="rounded bg-emerald-500/20 px-3 py-1 text-[11px] font-semibold text-emerald-600 hover:bg-emerald-500/30"
          >
            ✓ Approve &amp; Execute
          </button>
          <button
            onClick={onRejectPlan}
            className="rounded border border-border px-3 py-1 text-[11px] font-medium text-zinc-500 hover:text-zinc-800"
          >
            ✕ Reject
          </button>
        </div>
      )}
      {!isError && unanchored.length > 0 && (
        <div className="mt-2 space-y-1">
          {unanchored.map((t) => (
            <ToolCard key={t.id} event={t} />
          ))}
        </div>
      )}
      {!isError && message.steps && message.steps.length > 0 && (
        <StepTimeline steps={message.steps} />
      )}
      {!isError && message.diffs && message.diffs.length > 0 && (
        <div className="mt-2 space-y-1">
          {message.diffs.length > 1 && bulkResolved == null && (
            <div className="flex items-center gap-1.5 rounded border border-border bg-panel-2/60 px-2 py-1">
              <span className="text-[10px] text-zinc-400">Apply to all {message.diffs.length} files:</span>
              <button
                onClick={() => setBulkResolved("accepted")}
                className="rounded bg-emerald-500/15 px-2 py-0.5 text-[10px] font-medium text-emerald-600 hover:bg-emerald-500/25"
              >
                Accept All
              </button>
              <button
                onClick={(e) => {
                  const diffs = message.diffs ?? [];
                  void (async () => {
                    for (const d of diffs) {
                      if (!d.before) continue;
                      try {
                        await api.revertFile(d.path, d.before);
                      } catch {}
                    }
                    setBulkResolved("rejected");
                  })();
                  e.currentTarget.blur();
                }}
                className="rounded bg-red-500/15 px-2 py-0.5 text-[10px] font-medium text-red-600 hover:bg-red-500/25"
              >
                Reject All
              </button>
            </div>
          )}
          {bulkResolved != null ? (
            <p className="rounded border border-border bg-panel-2/60 px-2 py-1 text-[10px] text-zinc-500">
              {bulkResolved === "accepted"
                ? `Accepted all ${message.diffs.length} file diff${message.diffs.length !== 1 ? "s" : ""}.`
                : `Reverted all ${message.diffs.length} file diff${message.diffs.length !== 1 ? "s" : ""}.`}
            </p>
          ) : (
            message.diffs.map((d, di) => (
              <DiffView key={`${d.path}-${di}`} path={d.path} diff={d.diff ?? ""} before={d.before} />
            ))
          )}
        </div>
      )}
      {message.done && (
        <div
          className="mt-1 text-[9.5px] tabular-nums text-zinc-400"
          title={
            `${message.done.inputTokens} tok in · ${message.done.outputTokens} tok out` +
            (message.done.cacheReadTokens > 0
              ? ` · cache read ${message.done.cacheReadTokens}`
              : "") +
            (message.done.cacheWriteTokens > 0
              ? ` · cache write ${message.done.cacheWriteTokens}`
              : "") +
            (message.done.reasoningTokens > 0
              ? ` · reasoning ${message.done.reasoningTokens}`
              : "") +
            ` · ${message.done.generatedChars} chars`
          }
        >
          {outcomeLabel(message.done.outcome)} ·{" "}
          {(message.done.elapsedMs / 1000).toFixed(1)}s ·{" "}
          {message.done.outputTokens} tok out · {message.done.inputTokens} tok in ·{" "}
          {message.done.tokensPerSec.toFixed(1)} tok/s
          {message.done.stopReason !== "done" && ` · ${message.done.stopReason}`}
        </div>
      )}
    </div>
  );
}

export default function ChatPanel(props: ChatPanelProps) {  const {
    messages,
    streams,
    activeSessionId,
    isStreaming,
    status,
    lastDone,
    modelName,
    agentMode,
    onAgentModeChange,
    onSend,
    onCancel,
    onClear,
    currentStep,
    currentSubtask,
    verify,
    onVerifyChange,
    yolo = false,
    onYoloChange,
    skills = [],
    attachments = [],
    onAttachClick,
    onDetachFile,
    pendingPlan,
    onApprovePlan,
    onRejectPlan,
    onOpenSkills,
    width,
    todos,
    questionReq,
    onRespondQuestion,
    onEditResubmit,
    onExport,
    exportFormat = "pdf",
    onExportFormatChange,
    contextUsage,
    onDropFiles,
  } = props;
  const [input, setInput] = useState("");
  const [inputError, setInputError] = useState<string | null>(null);
  const scrollRef = useRef<VirtuosoHandle>(null);
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [isAtBottom, setIsAtBottom] = useState(true);
  const sendingRef = useRef(false);
  const historyRef = useRef<string[]>([]);
  const historyIdxRef = useRef<number>(-1);

  const streamingText = activeSessionId != null ? (streams.get(activeSessionId) ?? "") : "";
  const totalLen =
    messages.reduce((n, m) => n + m.content.length, 0) + streamingText.length;

  useEffect(() => {
    if (messages.length === 0) return;
    if (!isAtBottom) return;
    scrollRef.current?.scrollToIndex({ index: "LAST", behavior: "auto" });
  }, [totalLen, isAtBottom]);

  // Drag-and-drop file attachment (Tauri 2 window events).
  useEffect(() => {
    if (!onDropFiles) return;
    let unlisten: (() => void) | undefined;
    import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      getCurrentWindow()
        .onDragDropEvent((e) => {
          if (e.payload.type === "drop" && e.payload.paths.length > 0) {
            onDropFiles(e.payload.paths);
          }
        })
        .then((fn) => {
          unlisten = fn;
        });
    });
    return () => {
      unlisten?.();
    };
  }, [onDropFiles]);

  const submit = () => {
    const raw = input.trim();
    if (!raw || isStreaming || sendingRef.current) return;
    if (raw.length > MAX_INPUT_CHARS) {
      setInputError(
        `Message is too long (${raw.length.toLocaleString()} chars; max ${MAX_INPUT_CHARS.toLocaleString()}). Shorten it and try again.`,
      );
      return;
    }
    setInputError(null);
    sendingRef.current = true;

    // Editing mode: resubmit edited message, truncating history.
    if (editingIndex != null && onEditResubmit) {
      onEditResubmit(raw, editingIndex);
      setEditingIndex(null);
      setInput("");
      sendingRef.current = false;
      return;
    }

    let text = raw;
    let opts: SendOptions | undefined;

    if (raw.startsWith("/")) {
      const [cmd, ...rest] = raw.slice(1).split(" ");
      const arg = rest.join(" ").trim();
      switch (cmd) {
        case "plan":
          if (!arg) return;
          text = arg;
          opts = { planMode: true };
          break;
        case "act":
          text = arg || "Proceed with the approved plan.";
          opts = { verify };
          break;
        case "decompose":
          text = arg || "Decompose the approved task and complete it.";
          opts = { decompose: true, verify };
          break;
        case "fix":
          text = arg
            ? `Diagnose and fix: ${arg}`
            : "Diagnose the current state of the workspace and fix any problems you find.";
          opts = { verify: true };
          break;
        case "bug":
          text =
            "Investigate the reported bug carefully and produce a structured analysis. " +
            "Trace the symptom to its root cause, identify the exact failing file(s)/line(s), " +
            "explain the mechanism, then propose and apply a fix (use `analyze_bug` if available), " +
            "and verify it. Bug report:\n" +
            (arg || "A bug has been reported in the workspace — find it and fix it.");
          opts = { verify: true };
          break;
        case "review":
          text =
            "Perform a thorough code review of the target and report issues by severity: " +
            "correctness, concurrency, error-handling, security, performance and style. " +
            "Quote the relevant code, explain each finding, and suggest concrete fixes. " +
            "Target:\n" +
            (arg || "Review the workspace's most recently changed file(s).");
          opts = { verify: false };
          break;
        case "test":
          text = arg
            ? `Run the test suite and fix any failures: ${arg}`
            : "Run the project's test suite, then diagnose and fix any failures, then re-run to verify.";
          opts = { verify: true };
          break;
        case "commit":
          text = arg || "Create a git commit with a descriptive message summarizing the current uncommitted changes.";
          opts = { verify: false };
          break;
        case "skills":
          onOpenSkills();
          setInput("");
          return;
        case "clear":
          onClear();
          setInput("");
          return;
        default:
          break;
      }
    }

    onSend(text, opts);
    historyRef.current.push(raw);
    historyIdxRef.current = -1;
    setInput("");
    // Reset dedup guard after a tick so the UI state can propagate.
    setTimeout(() => { sendingRef.current = false; }, 0);
  };

  const showingHints = !isStreaming && input.trim().startsWith("/");

  // @-mention autocomplete: a trailing "@token" while typing filters skills.
  const mentionMatch = isStreaming ? null : input.match(/(?:^|\s)@([\w-]*)$/);
  const mentionHits = mentionMatch
    ? skills
        .filter((n) =>
          n.toLowerCase().includes(mentionMatch[1].toLowerCase()),
        )
        .slice(0, 6)
    : [];

  const insertMention = (name: string) => {
    setInput((prev) =>
      prev.replace(/(?:^|\s)@[\w-]*$/, (lead) => `${lead}@${name} `),
    );
  };

  // --- voice dictation (MediaRecorder → whisper) ---
  const [recording, setRecording] = useState(false);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const [transcribing, setTranscribing] = useState(false);

  const toggleDictation = async () => {
    if (recording) {
      recorderRef.current?.stop();
      return;
    }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const chunks: Blob[] = [];
      const recorder = new MediaRecorder(stream);
      recorderRef.current = recorder;
      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) chunks.push(e.data);
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((t) => t.stop());
        setRecording(false);
        setTranscribing(true);
        const blob = new Blob(chunks, { type: recorder.mimeType });
        blob
          .arrayBuffer()
          .then((buf) =>
            api.voiceTranscribeData(
              Array.from(new Uint8Array(buf)),
              "webm",
            ),
          )
          .then((text) => {
            if (text.trim()) {
              setInput((prev) => (prev ? `${prev} ${text.trim()}` : text.trim()));
            }
          })
          .catch(() => {})
          .finally(() => setTranscribing(false));
      };
      recorder.start();
      setRecording(true);
    } catch {
      // mic permission denied / unsupported — stay silent
    }
  };

  return (
    <aside
      className="flex h-full min-h-0 min-w-0 shrink-0 flex-col overflow-hidden border-l border-border bg-panel"
      style={width != null ? { width, minWidth: 300, maxWidth: 720 } : undefined}
    >
      <header className="flex min-h-9 shrink-0 flex-wrap items-center justify-between gap-x-2 gap-y-1 border-b border-border px-3 py-1">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
          Assistant
        </span>
        <div className="flex min-w-0 flex-wrap items-center gap-2">
          {agentMode && (
            <button
              onClick={() => onVerifyChange(!verify)}
              title="After edits, run tests/typecheck before finishing"
              className={`rounded px-1.5 py-0.5 text-[10px] font-semibold transition-colors ${
                verify
                  ? "bg-emerald-500/20 text-emerald-600"
                  : "border border-border text-zinc-500 hover:text-zinc-700"
              }`}
            >
              Verify
            </button>
          )}
          {agentMode && onYoloChange && (
            <button
              onClick={() => onYoloChange(!yolo)}
              title="YOLO: auto-approve routine shell commands (tests/builds/inspects). Red-zone stays blocked."
              className={`rounded px-1.5 py-0.5 text-[10px] font-bold transition-colors ${
                yolo
                  ? "bg-amber-500/25 text-amber-600"
                  : "border border-border text-zinc-500 hover:text-zinc-700"
              }`}
            >
              YOLO
            </button>
          )}
          <button
            onClick={() => onAgentModeChange(!agentMode)}
            title="Toggle agentic tool-use mode"
            className={`rounded px-2 py-0.5 text-[10px] font-semibold transition-colors ${
              agentMode
                ? "bg-cyan-500/20 text-cyan-600"
                : "border border-border text-zinc-500 hover:text-zinc-700"
            }`}
          >
            Agent
          </button>
          {currentStep != null && isStreaming && (
            <span className="shrink-0 rounded bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-semibold text-amber-600">
              step {currentStep}
            </span>
          )}
          {currentSubtask != null && isStreaming && (
            <span
              className="shrink-0 truncate rounded bg-violet-500/15 px-1.5 py-0.5 text-[10px] font-semibold text-violet-600"
              title={currentSubtask.title}
            >
              subtask {currentSubtask.index}/{currentSubtask.total}
            </span>
          )}
          <span className="max-w-24 truncate text-[10px] text-zinc-400">
            {modelName ?? "no model loaded"}
          </span>
          {onExport && messages.length > 0 && (
            <div className="flex items-center gap-1">
              {onExportFormatChange && (
                <select
                  value={exportFormat}
                  onChange={(e) => onExportFormatChange(e.target.value)}
                  className="rounded border border-border bg-transparent px-1 py-0.5 text-[10px] text-zinc-500 hover:border-accent/50"
                  title="Export format"
                >
                  <option value="pdf">PDF</option>
                  <option value="docx">DOCX</option>
                  <option value="csv">CSV</option>
                </select>
              )}
              <button
                onClick={onExport}
                title={`Export chat as ${exportFormat.toUpperCase()}`}
                className="rounded border border-border px-1.5 py-0.5 text-[10px] text-zinc-500 hover:border-accent/50 hover:text-accent"
              >
                ⤓ export
              </button>
            </div>
          )}
        </div>
      </header>

      {messages.length === 0 && !isStreaming && (
        <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2 text-[12.5px]">
          <p className="mt-6 text-center text-[11px] leading-relaxed text-zinc-400">
            Load a model and ask it anything.
            <br />
            Enter = send · Shift+Enter = newline
            <br />
            Try <span className="text-accent">/plan</span>,{" "}
            <span className="text-accent">/fix</span>,{" "}
            <span className="text-accent">/bug</span>,{" "}
            <span className="text-accent">/review</span>,{" "}
            <span className="text-accent">/test</span> or{" "}
            <span className="text-accent">/commit</span>
          </p>
        </div>
      )}
      {messages.length > 0 && (
        <Virtuoso
          ref={scrollRef}
          data={messages}
          className="min-h-0 flex-1 px-3 py-2 text-[12.5px]"
          followOutput={() => isAtBottom}
          atBottomStateChange={(isBottom) => setIsAtBottom(isBottom)}
          itemContent={(i, m) =>
            m.role === "user" ? (
              <div key={i} className="group mb-2 flex justify-end">
                <div className="flex max-w-[85%] items-end gap-1">
                  <div
                    className={`whitespace-pre-wrap rounded-lg px-2.5 py-1.5 text-left text-zinc-800 ${
                      editingIndex === i
                        ? "ring-2 ring-accent bg-accent/20"
                        : "bg-accent/15"
                    }`}
                  >
                    {m.content}
                  </div>
                  {!isStreaming && editingIndex == null && (
                    <button
                      onClick={() => {
                        setEditingIndex(i);
                        setInput(m.content);
                        scrollRef.current?.scrollToIndex({
                          index: "LAST",
                          align: "end",
                          behavior: "smooth",
                        });
                      }}
                      title="Edit & resubmit"
                      className="shrink-0 rounded border border-transparent px-1 py-0.5 text-[10px] text-zinc-400 opacity-0 transition-opacity hover:border-border hover:text-zinc-600 group-hover:opacity-100"
                    >
                      ✎
                    </button>
                  )}
                  {editingIndex === i && (
                    <button
                      onClick={() => {
                        setEditingIndex(null);
                        setInput("");
                      }}
                      title="Cancel edit"
                      className="shrink-0 rounded border border-transparent px-1 py-0.5 text-[10px] text-red-400 hover:border-border hover:text-red-600"
                    >
                      ✕
                    </button>
                  )}
                </div>
              </div>
            ) : (
              <AssistantTurn
                key={i}
                message={m}
                live={isStreaming && m.sessionId === activeSessionId}
                liveText={m.sessionId === activeSessionId ? streamingText : ""}
                pendingPlan={
                  pendingPlan && m.sessionId === pendingPlan.sessionId && !isStreaming
                    ? pendingPlan
                    : null
                }
                onApprovePlan={onApprovePlan}
                onRejectPlan={onRejectPlan}
              />
            )
          }
        />
      )}

      {questionReq && (
        <QuestionCard request={questionReq} onRespond={onRespondQuestion} />
      )}

      {todos && todos.items.length > 0 && (
        <div className="shrink-0 border-t border-border px-3 py-2">
          <TodoCard todos={todos} />
        </div>
      )}

      {contextUsage && (contextUsage.totalTokens > contextUsage.threshold * 0.7 || contextUsage.overflow) && (
        <div className={`shrink-0 border-t px-3 py-1.5 text-[10px] ${
          contextUsage.overflow
            ? "border-amber-300 bg-amber-50 text-amber-700"
            : contextUsage.totalTokens > contextUsage.threshold * 0.9
              ? "border-red-300 bg-red-50 text-red-600"
              : "border-amber-200 bg-amber-50/50 text-amber-600"
        }`}>
          {contextUsage.overflow ? (
            <span>⚠ Context budget exceeded — oldest turns are being evicted ({contextUsage.evictedTurns} evicted)</span>
          ) : (
            <span>⚠ Context pressure: {Math.round(contextUsage.totalTokens / contextUsage.threshold * 100)}% of eviction threshold</span>
          )}
        </div>
      )}

      <StatusIndicator status={status} />

      <footer className="shrink-0 border-t border-border p-2">
        {attachments.length > 0 && (
          <div className="mb-1.5 flex flex-wrap gap-1">
            {attachments.map((a) => {
              const name = a.path.split(/[\\/]/).pop() ?? a.path;
              return (
                <span
                  key={a.path}
                  className="flex max-w-full items-center gap-1 rounded border border-accent/40 bg-accent/10 px-1.5 py-0.5 text-[10px] text-cyan-700"
                  title={`${a.path} — ${a.chunkCount} chunk(s) indexed`}
                >
                  <button
                    onClick={() => onDetachFile?.(a.path)}
                    aria-label={`Detach ${name}`}
                    className="font-mono text-cyan-700 hover:text-red-500"
                  >
                    {name} ✕
                  </button>
                </span>
              );
            })}
          </div>
        )}
        {mentionHits.length > 0 && (
          <div className="mb-1.5 flex flex-col rounded-md border border-border bg-panel-2 p-1 shadow-lg">
            <span className="px-1.5 pb-0.5 text-[9px] font-semibold uppercase tracking-wide text-zinc-400">
              Skills — @mention to activate
            </span>
            {mentionHits.map((n) => (
              <button
                key={n}
                onClick={() => insertMention(n)}
                className="rounded px-1.5 py-1 text-left text-[11.5px] text-zinc-700 hover:bg-accent/10 hover:text-accent"
              >
                <span className="font-mono">@{n}</span>
              </button>
            ))}
          </div>
        )}
        {showingHints && (
          <div className="mb-1.5 flex flex-wrap gap-1">
            {SLASH_HINTS.map((s) => (
              <button
                key={s.cmd}
                onClick={() => setInput(s.cmd + " ")}
                className="rounded border border-border px-1.5 py-0.5 text-[10px] text-zinc-500 hover:border-accent/50 hover:text-accent"
                title={s.hint}
              >
                {s.cmd}
              </button>
            ))}
          </div>
        )}
        <textarea
          value={input}
          maxLength={MAX_INPUT_CHARS}
          onChange={(e) => {
            setInput(e.target.value);
            if (inputError) setInputError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
            if (e.key === "Escape" && editingIndex != null) {
              setEditingIndex(null);
              setInput("");
            }
            // Ctrl+Up / Ctrl+Down: cycle through sent message history.
            if (e.key === "ArrowUp" && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              const hist = historyRef.current;
              if (hist.length === 0) return;
              if (historyIdxRef.current === -1) {
                historyIdxRef.current = hist.length - 1;
              } else if (historyIdxRef.current > 0) {
                historyIdxRef.current--;
              }
              setInput(hist[historyIdxRef.current]);
            }
            if (e.key === "ArrowDown" && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              const hist = historyRef.current;
              if (historyIdxRef.current === -1) return;
              if (historyIdxRef.current < hist.length - 1) {
                historyIdxRef.current++;
                setInput(hist[historyIdxRef.current]);
              } else {
                historyIdxRef.current = -1;
                setInput("");
              }
            }
          }}
          placeholder={
            editingIndex != null
              ? "Editing message… Enter to resubmit"
              : isStreaming
                ? "Generating…"
                : modelName
                  ? "Ask the model… (try /plan)"
                  : "Load a model first…"
          }
          rows={3}
          disabled={!modelName}
          className="w-full resize-none rounded-md border border-border bg-panel-2 px-2.5 py-2 text-[12.5px] text-ink outline-none placeholder:text-zinc-500 focus:border-accent/60 disabled:opacity-50"
        />
        {inputError && (
          <p className="mt-1 rounded border border-amber-400/40 bg-amber-50 px-2 py-1 text-[10px] leading-snug text-amber-700">
            {inputError}
          </p>
        )}
        <div className="mt-2 flex items-center justify-between">
          {onAttachClick && (
            <button
              onClick={onAttachClick}
              title="Attach a text file for semantic search (RAG)"
              className="mr-1 rounded border border-border px-1.5 py-0.5 text-[11px] text-zinc-500 hover:border-accent/50 hover:text-accent"
            >
              📎
            </button>
          )}
          <button
            onClick={() => void toggleDictation()}
            disabled={transcribing}
            title={
              transcribing
                ? "Transcribing…"
                : recording
                  ? "Stop recording and transcribe"
                  : "Dictate (requires local whisper + ffmpeg)"
            }
            className={`mr-1 rounded border px-1.5 py-0.5 text-[11px] transition-colors ${
              recording
                ? "border-red-400 bg-red-500/15 text-red-600"
                : "border-border text-zinc-500 hover:border-accent/50 hover:text-accent"
            } disabled:opacity-50`}
          >
            {recording ? "■" : transcribing ? "…" : "🎤"}
          </button>
          {isStreaming ? (
            <button
              onClick={onCancel}
              className="rounded bg-red-500/15 px-3 py-1 text-[11px] font-medium text-red-600 hover:bg-red-500/25"
            >
              ■ Stop
            </button>
          ) : (
            <button
              onClick={submit}
              disabled={!input.trim() || !modelName}
              className="rounded bg-accent px-3.5 py-1 text-[11px] font-semibold text-white hover:bg-cyan-500 disabled:opacity-40"
            >
              {editingIndex != null ? "Resubmit" : "Send"}
            </button>
          )}
          {lastDone && !isStreaming && (
            <span
              className="text-[10px] tabular-nums text-zinc-400"
              title={
                `in ${lastDone.inputTokens} tok · out ${lastDone.outputTokens} tok` +
                (lastDone.cacheReadTokens > 0
                  ? ` · cache read ${lastDone.cacheReadTokens}`
                  : "") +
                (lastDone.cacheWriteTokens > 0
                  ? ` · cache write ${lastDone.cacheWriteTokens}`
                  : "") +
                (lastDone.reasoningTokens > 0
                  ? ` · reasoning ${lastDone.reasoningTokens}`
                  : "") +
                ` · ${lastDone.generatedChars} chars`
              }
            >
              {outcomeLabel(lastDone.outcome)} · {lastDone.outputTokens} tok out ·{" "}
              {lastDone.inputTokens} tok in · {lastDone.tokensPerSec.toFixed(1)} tok/s ·{" "}
              {lastDone.stopReason}
            </span>
          )}
        </div>
      </footer>
    </aside>
  );
}
