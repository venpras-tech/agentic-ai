import { useEffect, useRef, useState } from "react";

import type {
  AgentToolEvent,
  ChatMessage,
  InferenceDone,
  StepTimelineStep,
} from "../types";
import DiffView from "./DiffView";

export interface SendOptions {
  planMode?: boolean;
  verify?: boolean;
  decompose?: boolean;
}

interface ChatPanelProps {
  messages: ChatMessage[];
  streams: ReadonlyMap<number, string>;
  activeSessionId: number | null;
  isStreaming: boolean;
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
  pendingPlan: { sessionId: number; planText: string } | null;
  onApprovePlan: () => void;
  onRejectPlan: () => void;
  onOpenSkills: () => void;
}

const SLASH_HINTS: { cmd: string; hint: string }[] = [
  { cmd: "/plan", hint: "draft a plan, then approve to execute" },
  { cmd: "/act", hint: "execute a task with tools" },
  { cmd: "/decompose", hint: "break a large task into subtasks, then execute" },
  { cmd: "/fix", hint: "diagnose, fix and verify" },
  { cmd: "/test", hint: "run tests and fix failures" },
  { cmd: "/commit", hint: "checkpoint git commit" },
  { cmd: "/skills", hint: "open knowledge panel" },
  { cmd: "/clear", hint: "clear conversation" },
];

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
  return (
    <div className="rounded-md border border-border bg-panel-2/70 px-2 py-1.5">
      <div className="flex items-center gap-1.5 text-[10.5px]">
        <span
          className={`h-1.5 w-1.5 shrink-0 rounded-full ${
            running ? "animate-pulse bg-amber-400" : failed ? "bg-red-400" : "bg-emerald-400"
          }`}
        />
        <span className="truncate font-medium text-zinc-700">
          {event.tool.replaceAll("_", " ")}
        </span>
        <span className="ml-auto shrink-0 text-zinc-400">
          {running
            ? "running…"
            : event.durationMs != null
              ? `${event.durationMs}ms`
              : event.status}
        </span>
      </div>
      <p className="mt-0.5 text-[10.5px] leading-snug text-zinc-500">
        {event.summary}
      </p>
      {event.output && (event.output.trim()?.length ?? 0) > 0 && (
        <pre className="mt-1 max-h-28 overflow-auto whitespace-pre-wrap rounded bg-zinc-100 px-1.5 py-1 font-mono text-[10px] leading-snug text-zinc-500">
          {event.output}
        </pre>
      )}
      {event.detail && (
        <p className="mt-0.5 max-h-24 overflow-auto whitespace-pre-wrap text-[10px] leading-snug text-red-600/80">
          {event.detail}
        </p>
      )}
    </div>
  );
}

export default function ChatPanel(props: ChatPanelProps) {
  const {
    messages,
    streams,
    activeSessionId,
    isStreaming,
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
    pendingPlan,
    onApprovePlan,
    onRejectPlan,
    onOpenSkills,
  } = props;
  const [input, setInput] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);

  const streamingText = activeSessionId != null ? (streams.get(activeSessionId) ?? "") : "";
  const totalLen =
    messages.reduce((n, m) => n + m.content.length, 0) + streamingText.length;

  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [totalLen]);

  const submit = () => {
    const raw = input.trim();
    if (!raw || isStreaming) return;
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
    setInput("");
  };

  const showingHints = !isStreaming && input.trim().startsWith("/");

  return (
    <aside className="flex w-96 min-w-80 shrink-0 flex-col border-l border-border bg-panel">
      <header className="flex h-9 shrink-0 items-center justify-between gap-2 border-b border-border px-3">
        <span className="text-[11px] font-semibold uppercase tracking-wider text-zinc-500">
          Assistant
        </span>
        <div className="flex min-w-0 items-center gap-2">
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
        </div>
      </header>

      <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-3 py-2 text-[12.5px]">
        {messages.length === 0 && !isStreaming && (
          <p className="mt-6 text-center text-[11px] leading-relaxed text-zinc-400">
            Load a model and ask it anything.
            <br />
            Enter = send · Shift+Enter = newline
            <br />
            Try <span className="text-accent">/plan</span>,{" "}
            <span className="text-accent">/fix</span>,{" "}
            <span className="text-accent">/test</span> or{" "}
            <span className="text-accent">/commit</span>
          </p>
        )}
        {messages.map((m, i) =>
          m.role === "user" ? (
            <div key={i} className="mb-2 flex justify-end">
              <div className="max-w-[85%] whitespace-pre-wrap rounded-lg bg-accent/15 px-2.5 py-1.5 text-left text-zinc-800">
                {m.content}
              </div>
            </div>
          ) : (
            <div key={i} className="mb-3">
              <div className="mb-0.5 flex items-center gap-2 text-[10px] uppercase tracking-wider text-zinc-400">
                <span>{m.role === "error" ? "error" : "assistant"}</span>
                {m.done && m.role !== "error" && <OutcomeBadge outcome={m.done.outcome} />}
              </div>
              <div
                className={`whitespace-pre-wrap leading-relaxed ${
                  m.role === "error" ? "text-red-600" : "text-ink"
                }`}
              >
                {m.content || "…"}
              </div>
              {pendingPlan && m.sessionId === pendingPlan.sessionId && !isStreaming && (
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
              {m.tools && m.tools.length > 0 && (
                <div className="mt-2 space-y-1">
                  {m.tools.map((t) => (
                    <ToolCard key={t.id} event={t} />
                  ))}
                </div>
              )}
              {m.steps && m.steps.length > 0 && <StepTimeline steps={m.steps} />}
              {m.diffs && m.diffs.length > 0 && (
                <div className="mt-2 space-y-1">
                  {m.diffs.map((d, di) => (
                    <DiffView key={`${d.path}-${di}`} path={d.path} diff={d.diff ?? ""} />
                  ))}
                </div>
              )}
            </div>
          ),
        )}
        {isStreaming && (
          <div className="whitespace-pre-wrap leading-relaxed">
            {streamingText}
            <span className="animate-pulse text-accent">▌</span>
          </div>
        )}
      </div>

      <footer className="shrink-0 border-t border-border p-2">
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
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder={
            isStreaming
              ? "Generating…"
              : modelName
                ? "Ask the model… (try /plan)"
                : "Load a model first…"
          }
          rows={3}
          disabled={!modelName}
          className="w-full resize-none rounded-md border border-border bg-panel-2 px-2.5 py-2 text-[12.5px] text-ink outline-none placeholder:text-zinc-500 focus:border-accent/60 disabled:opacity-50"
        />
        <div className="mt-2 flex items-center justify-between">
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
              Send
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
