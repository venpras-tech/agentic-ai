import { memo, type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { Virtuoso, type VirtuosoHandle } from "react-virtuoso";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { createPortal } from "react-dom";
import { parseChatSegments } from "../lib/chatParse";
import type { ChatSegment } from "../lib/chatParse";
import { planRewind, restoreRewind, type RewindPlan } from "../lib/rewind";
import { useShell, useChat } from "../context/AppContext";
import { useAgentStreamContext } from "../context/AgentStreamContext";
import { api } from "../lib/ipc";
import { isDefaultChat, newChatId, splitConversationId } from "../lib/sessionLog";
import ReasoningAccordion from "./chat/ReasoningAccordion";
import ToolExecutionBadge from "./chat/ToolExecutionBadge";
import AgentStageIndicator from "./chat/AgentStageIndicator";
import { isStageVisible, stageOrQueued } from "../lib/agentStages";
import type { ToolStatus } from "./chat/ToolExecutionBadge";
import type { ChatMessage } from "../context/types";
import DiffViewer from "./chat/DiffViewer";
import CodeBlock from "./chat/CodeBlock";
import "./MessageList.css";

export type { ChatMessage };

export interface LiveToolState {
  id: string;
  tool: string;
  status: string;
  summary: string;
  output?: string;
  durationMs?: number;
  exitCode?: number;
}

function EditIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
      <path d="M12 20h9" />
      <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4 12.5-12.5z" />
    </svg>
  );
}

function CopyIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
      <rect x="9" y="9" width="11" height="11" rx="1.5" />
      <path d="M5 15V5a1.5 1.5 0 0 1 1.5-1.5H15" />
    </svg>
  );
}

function CheckIcon() {
  return (
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden>
      <path d="M20 6 9 17l-5-5" />
    </svg>
  );
}

// Matches `path/to/file.ext:12` or `file.ext:12-34`. Requires a file-like
// tail (extension or a path separator) so prose like "note:1" is ignored.
const PATH_LINE_RE = /((?:[\w.@-]+[/\\])*[\w.@-]+\.[A-Za-z0-9]+):(\d+)(?:-(\d+))?/g;

function resolveWorkspacePath(rel: string, projectPath: string): string {
  if (!rel || /^[A-Za-z]:/.test(rel) || rel.startsWith("/") || rel.startsWith("\\\\")) {
    return rel;
  }
  return `${projectPath.replace(/[/\\]+$/, "")}/${rel}`;
}

/** Split a plain string into text and clickable `path:line` link nodes. */
function linkifyText(text: string, projectPath: string, keyPrefix: string): ReactNode[] {
  if (!text || !PATH_LINE_RE.test(text)) return [text];
  PATH_LINE_RE.lastIndex = 0;
  const out: ReactNode[] = [];
  let last = 0;
  let m: RegExpExecArray | null;
  let i = 0;
  while ((m = PATH_LINE_RE.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const [, path, line, endLine] = m;
    const target = resolveWorkspacePath(path, projectPath);
    const label = endLine ? `${path}:${line}-${endLine}` : `${path}:${line}`;
    out.push(
      <button
        key={`${keyPrefix}-pl-${i++}`}
        type="button"
        className="msg-pathlink"
        title={`Open ${path} at line ${line}`}
        onClick={() =>
          window.dispatchEvent(
            new CustomEvent("vencoder-open-editor", {
              detail: { path: target, line: Number(line) },
            }),
          )
        }
      >
        {label}
      </button>,
    );
    last = m.index + m[0].length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

/** Recursively linkify string children without disturbing React elements. */
function linkifyChildren(
  children: ReactNode,
  projectPath: string,
  keyPrefix = "nl",
): ReactNode {
  if (typeof children === "string") {
    const nodes = linkifyText(children, projectPath, keyPrefix);
    return nodes.length === 1 ? nodes[0] : nodes;
  }
  if (Array.isArray(children)) {
    return children.map((c, i) =>
      typeof c === "string"
        ? linkifyText(c, projectPath, `${keyPrefix}-${i}`)
        : c,
    );
  }
  return children;
}

function buildMarkdownComponents(projectPath: string) {
  return {
    code: (props: {
      node?: unknown;
      className?: string;
      children?: ReactNode;
      inline?: boolean;
    }) => {
      if (props.className === "language-diff") {
        return <DiffViewer content={String(props.children ?? "")} />;
      }
      // Inline code that looks like `path:line` becomes a jump link.
      if (props.inline && typeof props.children === "string") {
        const nodes = linkifyText(props.children, projectPath, "code");
        if (nodes.length > 1) return <span className="msg-inline-link">{nodes}</span>;
      }
      return <CodeBlock {...props} />;
    },
    p: ({ children }: { children?: ReactNode }) => (
      <p className="msg-p">{linkifyChildren(children, projectPath)}</p>
    ),
    ul: ({ children }: { children?: ReactNode }) => <ul className="msg-ul">{children}</ul>,
    ol: ({ children }: { children?: ReactNode }) => <ol className="msg-ol">{children}</ol>,
    li: ({ children }: { children?: ReactNode }) => <li className="msg-li">{children}</li>,
    a: ({ href, children }: { href?: string; children?: ReactNode }) => (
      <a className="msg-link" href={href} target="_blank" rel="noreferrer noopener">
        {children}
      </a>
    ),
    blockquote: ({ children }: { children?: ReactNode }) => (
      <blockquote className="msg-quote">{children}</blockquote>
    ),
    h1: ({ children }: { children?: ReactNode }) => <h3 className="msg-h msg-h1">{children}</h3>,
    h2: ({ children }: { children?: ReactNode }) => <h4 className="msg-h msg-h2">{children}</h4>,
    h3: ({ children }: { children?: ReactNode }) => <h5 className="msg-h msg-h3">{children}</h5>,
    hr: () => <hr className="msg-hr" />,
    table: ({ children }: { children?: ReactNode }) => (
      <div className="msg-table-wrap">
        <table className="msg-table">{children}</table>
      </div>
    ),
  };
}

function SegmentView({
  segment,
  isLast,
  streaming,
  statusByPath,
  projectPath,
}: {
  segment: ChatSegment;
  isLast: boolean;
  streaming: boolean;
  statusByPath: Record<string, string>;
  projectPath: string;
}) {
  const mdComponents = useMemo(() => buildMarkdownComponents(projectPath), [projectPath]);
  switch (segment.kind) {
    case "reasoning":
      return (
        <ReasoningAccordion text={segment.text} streaming={isLast ? streaming : false} />
      );
    case "tool":
      return (
        <ToolExecutionBadge
          name={segment.name}
          status={segment.status as ToolStatus}
          output={segment.text}
          streaming={isLast ? streaming : false}
          durationMs={segment.durationMs}
          exitCode={segment.exitCode}
          args={segment.args}
        />
      );
    case "diff": {
      const rel = (segment.path || "").replaceAll("\\", "/");
      const status = statusByPath[rel] ?? statusByPath[segment.path ?? ""];
      const abs =
        !rel || /^[A-Za-z]:/.test(rel) || rel.startsWith("/")
          ? rel
          : `${projectPath.replace(/[/\\]+$/, "")}/${rel}`;
      return <DiffViewer path={segment.path} content={segment.text} status={status} openPath={abs} />;
    }
    case "gate":
      return (
        <div className="chat-gate-marker">
          <span className="chat-gate-chip">⛔ approval requested</span>
        </div>
      );
    case "text":
      if (!segment.text.trim()) return null;
      return (
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={mdComponents}>
          {segment.text}
        </ReactMarkdown>
      );
    default:
      return null;
  }
}

function isActionSegment(segment: ChatSegment): boolean {
  return segment.kind === "reasoning" || segment.kind === "tool" || segment.kind === "diff";
}

function useElapsedSeconds(running: boolean): number {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    if (!running) return;
    setSeconds(0);
    const id = window.setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => window.clearInterval(id);
  }, [running]);
  return seconds;
}

function StepGroup({
  segments,
  streaming,
  statusByPath,
  projectPath,
}: {
  segments: ChatSegment[];
  streaming: boolean;
  statusByPath: Record<string, string>;
  projectPath: string;
}) {
  const [open, setOpen] = useState(streaming);
  const elapsed = useElapsedSeconds(streaming);

  useEffect(() => {
    if (streaming) setOpen(true);
  }, [streaming]);

  const tools = segments.filter((s) => s.kind === "tool");
  const edits = segments.filter((s) => s.kind === "diff").length;
  const failed = tools.some((s) => s.kind === "tool" && (s.status === "error" || s.status === "failed"));

  const summary = [
    `${segments.length} step${segments.length === 1 ? "" : "s"}`,
    tools.length ? `${tools.length} tool${tools.length === 1 ? "" : "s"}` : null,
    edits ? `${edits} edit${edits === 1 ? "" : "s"}` : null,
    streaming && elapsed > 0 ? `${elapsed}s` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <div
      className={`chat-steps${open ? " open" : ""}${streaming ? " running" : ""}${failed ? " failed" : ""}`}
    >
      <button
        type="button"
        className="chat-steps-head"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        title={open ? "Hide steps" : "Show steps"}
      >
        <span className="chat-steps-glyph" aria-hidden="true">
          {streaming ? (
            <span className="tool-badge-spinner" />
          ) : failed ? (
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4">
              <path d="M12 8v5" strokeLinecap="round" />
              <circle cx="12" cy="16.5" r="0.6" fill="currentColor" stroke="none" />
            </svg>
          ) : (
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6">
              <path d="M5 12.5l4.5 4.5L19 7" strokeLinecap="round" strokeLinejoin="round" />
            </svg>
          )}
        </span>
        <span className="chat-steps-title">{streaming ? "Working" : "Worked"}</span>
        <span className="chat-steps-summary">{summary}</span>
        <span className={`chat-steps-chevron${open ? " open" : ""}`} aria-hidden="true">
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.6">
            <path d="M9 6l6 6-6 6" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </span>
      </button>
      {open && (
        <div className="chat-steps-body">
          {segments.map((seg, i) => (
            <SegmentView
              key={i}
              segment={seg}
              isLast={streaming && i === segments.length - 1}
              streaming={streaming}
              statusByPath={statusByPath}
              projectPath={projectPath}
            />
          ))}
        </div>
      )}
    </div>
  );
}

function Segments({
  segments,
  streaming,
  statusByPath,
  projectPath,
}: {
  segments: ChatSegment[];
  streaming: boolean;
  statusByPath: Record<string, string>;
  projectPath: string;
}) {
  const lastReal = segments.map((s) => s.kind !== "gate").lastIndexOf(true);
  const nodes: ReactNode[] = [];
  let runStart = -1;

  const flush = (end: number) => {
    if (runStart < 0) return;
    const start = runStart;
    runStart = -1;
    const run = segments.slice(start, end);
    nodes.push(
      <StepGroup
        key={`g${start}`}
        segments={run}
        streaming={streaming && lastReal >= start && lastReal < end}
        statusByPath={statusByPath}
        projectPath={projectPath}
      />,
    );
  };

  segments.forEach((seg, i) => {
    if (isActionSegment(seg)) {
      if (runStart < 0) runStart = i;
      return;
    }
    flush(i);
    nodes.push(
      <SegmentView
        key={`s${i}`}
        segment={seg}
        isLast={i === lastReal}
        streaming={streaming}
        statusByPath={statusByPath}
        projectPath={projectPath}
      />,
    );
  });
  flush(segments.length);

  return <>{nodes}</>;
}

const REWIND_OPTIONS = [
  {
    id: "both",
    label: "Restore code and conversation",
    hint: "Revert the file edits from this turn and drop the messages after it.",
  },
  {
    id: "conversation",
    label: "Restore conversation only",
    hint: "Keep the current files, rewind the chat to this point.",
  },
  {
    id: "code",
    label: "Restore code only",
    hint: "Revert the file edits from this turn, keep every message.",
  },
] as const;

type RewindMode = (typeof REWIND_OPTIONS)[number]["id"];

/**
 * Per-turn rewind menu (Claude Code /rewind parity). The anchor is the earliest
 * timestamp at which restoring is still possible, so a restore that also drops
 * messages leaves the list rewindable to the same point again.
 */
function RewindMenu({
  index,
  boundaryTs,
  onClose,
  onRewound,
}: {
  index: number;
  boundaryTs: number;
  onClose: () => void;
  onRewound: (mode: RewindMode, restored: number) => void;
}) {
  const { truncateMessages, addActivity } = useChat();
  const [plan, setPlan] = useState<RewindPlan | null>(null);
  const [mode, setMode] = useState<RewindMode | null>(null);
  const [busy, setBusy] = useState(false);
  const boxRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    planRewind(boundaryTs)
      .then((p) => {
        if (live) setPlan(p);
      })
      .catch(() => {
        if (live) setPlan({ boundaryTs, checkpoints: [] });
      });
    return () => {
      live = false;
    };
  }, [boundaryTs]);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (boxRef.current && !boxRef.current.contains(e.target as Node)) onClose();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("mousedown", onDown);
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("mousedown", onDown);
      window.removeEventListener("keydown", onKey);
    };
  }, [onClose]);

  const files = plan?.checkpoints.length ?? 0;
  const filesLabel = plan == null ? "checking edits…" : `${files} file${files === 1 ? "" : "s"}`;
  const confirmable =
    plan != null &&
    !busy &&
    ((mode === "code" || mode === "both") ? files > 0 : true);

  const run = async () => {
    if (!mode || !plan) return;
    setBusy(true);
    try {
      let restored = 0;
      if (mode === "code" || mode === "both") {
        restored = await restoreRewind(plan);
      }
      if (mode === "conversation" || mode === "both") {
        truncateMessages(index);
      }
      onRewound(mode, restored);
    } catch (err) {
      addActivity(
        `Rewind failed: ${err instanceof Error ? err.message : String(err)}`,
        "error",
      );
      setBusy(false);
    }
  };

  return createPortal(
    <div className="rw-overlay" role="dialog" aria-label="Rewind to this point">
      <div className="rw-modal" ref={boxRef}>
        <div className="rw-head">
          <span className="rw-title">Rewind to this point</span>
          <button type="button" className="rw-close" onClick={onClose} aria-label="Close">
            ✕
          </button>
        </div>
        <p className="rw-sub">
          Everything from <strong>this message</strong> onward is affected. The agent
          changed <strong>{filesLabel}</strong>.
        </p>
        <div className="rw-options">
          {REWIND_OPTIONS.map((o) => (
            <label key={o.id} className={`rw-option${mode === o.id ? " selected" : ""}`}>
              <input
                type="radio"
                name="rw-mode"
                checked={mode === o.id}
                onChange={() => setMode(o.id)}
              />
              <span className="rw-option-body">
                <span className="rw-option-label">{o.label}</span>
                <span className="rw-option-hint">{o.hint}</span>
              </span>
            </label>
          ))}
        </div>
        <div className="rw-actions">
          <span className="rw-note">
            {(mode === "code" || mode === "both") && plan != null && files === 0
              ? "No agent edits were recorded after this point."
              : "Rewind does not undo shell-command changes — use git for those."}
          </span>
          <button
            type="button"
            className="rw-btn rw-confirm"
            onClick={() => void run()}
            disabled={!confirmable}
          >
            {busy ? "Rewinding…" : "Rewind"}
          </button>
          <button type="button" className="rw-btn" onClick={onClose}>
            Cancel
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

const Message = memo(function Message({
  message,
  index,
  statusByPath,
  projectPath,
  onFork,
  onRetry,
  onSummarize,
}: {
  message: ChatMessage;
  index: number;
  statusByPath: Record<string, string>;
  projectPath: string;
  onFork?: () => void;
  onRetry?: () => void;
  onSummarize?: () => void;
}) {
  const { role, content, streaming } = message;
  const { addActivity } = useChat();
  const [copied, setCopied] = useState(false);
  const [showTrace, setShowTrace] = useState(false);
  const [showRewind, setShowRewind] = useState(false);
  const mdComponents = useMemo(() => buildMarkdownComponents(projectPath), [projectPath]);

  const timeStr = () => {
    const d = new Date(message.ts || Date.now());
    return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  };

  const doneStats = message.done
    ? [
        message.done.outputTokens != null ? `${message.done.outputTokens} tok out` : null,
        message.done.inputTokens != null ? `${message.done.inputTokens} tok in` : null,
        message.done.tokensPerSec ? `${message.done.tokensPerSec.toFixed(1)} tok/s` : null,
        message.done.elapsedMs ? `${(message.done.elapsedMs / 1000).toFixed(1)}s` : null,
        message.done.stopReason && message.done.stopReason !== "done" ? message.done.stopReason : null,
      ]
        .filter(Boolean)
        .join(" · ")
    : null;

  const copyResponse = async () => {
    let text = content || "";
    if (message.role === "assistant") {
      const clean = parseChatSegments(content || "")
        .map((s) => {
          switch (s.kind) {
            case "text":
              return s.text.trim();
            case "reasoning":
              return s.text.trim() ? `[reasoning] ${s.text.trim()}` : "";
            case "tool":
              return s.text.trim() ? `[${s.name}: ${s.status}] ${s.text.trim()}` : `[${s.name}: ${s.status}]`;
            case "diff":
              return `[diff ${s.path}]\n${s.text.trimEnd()}`;
            default:
              return "";
          }
        })
        .filter(Boolean)
        .join("\n\n");
      if (clean) text = clean;
    }
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      // clipboard unavailable — ignore
    }
  };

  const editRequest = () => {
    window.dispatchEvent(
      new CustomEvent("vencoder-edit-request", { detail: { index, text: content || "" } }),
    );
  };

  const segments = useMemo<ChatSegment[]>(
    () => (role === "assistant" ? parseChatSegments(content || "") : []),
    [role, content],
  );

  // Per-turn change summary (Cursor-style pill): files touched and +/− line
  // counts, derived from the diff segments the backend embedded in the reply.
  const changes = useMemo(() => {
    const byPath = new Map<string, { adds: number; dels: number; status?: string }>();
    for (const seg of segments) {
      if (seg.kind !== "diff") continue;
      const rel = (seg.path || "").replaceAll("\\", "/");
      let adds = 0;
      let dels = 0;
      for (const raw of seg.text.split("\n")) {
        if (/^\+/.test(raw) && !/^\+\+\+/.test(raw)) adds++;
        else if (/^-/.test(raw) && !/^---/.test(raw)) dels++;
      }
      const status = statusByPath[rel] ?? statusByPath[seg.path ?? ""];
      const prev = byPath.get(rel);
      if (prev) {
        prev.adds += adds;
        prev.dels += dels;
        prev.status = status ?? prev.status;
      } else {
        byPath.set(rel, { adds, dels, status });
      }
    }
    return [...byPath.entries()].map(([path, v]) => ({ path, ...v }));
  }, [segments, statusByPath]);

  const [changesOpen, setChangesOpen] = useState(false);

  const roleLabel =
    role === "user"
      ? "You"
      : message.model
        ? message.model.provider === "Local"
          ? message.model.name
          : message.model.name
        : "Assistant";

  return (
    <article className={`msg-turn msg-turn--${role}${streaming ? " is-streaming" : ""}`}>
      <div className="msg-avatar" aria-hidden>
        {role === "user" ? (
          <span className="msg-avatar-icon msg-avatar-user">Y</span>
        ) : (
          <span className="msg-avatar-icon msg-avatar-ai">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M12 3l1.5 4.5L18 9l-4.5 1.5L12 15l-1.5-4.5L6 9l4.5-1.5L12 3z" />
            </svg>
          </span>
        )}
      </div>
      <div className="msg-body">
        <header className="msg-meta">
          <span className="msg-role">{roleLabel}</span>
          <span className="msg-dot">·</span>
          <time className="msg-time">{timeStr()}</time>
          {role === "assistant" && message.model && !streaming && message.model.provider !== "Local" && (
            <span className="msg-provider" title={message.model.provider}>{message.model.provider}</span>
          )}
          {role === "assistant" && changes.length > 0 && (
            <button
              type="button"
              className="msg-changes-pill"
              onClick={() => setChangesOpen((v) => !v)}
              title="Files changed this turn"
            >
              <span>{changes.length} file{changes.length === 1 ? "" : "s"}</span>
              <span className="msg-changes-add">+{changes.reduce((n, c) => n + c.adds, 0)}</span>
              <span className="msg-changes-del">−{changes.reduce((n, c) => n + c.dels, 0)}</span>
            </button>
          )}
          <span className="msg-actions">
            {onFork && !streaming && (
              <button
                type="button"
                className="msg-action"
                onClick={onFork}
                title="Fork this chat from this message"
              >
                Fork from here
              </button>
            )}
            {!streaming && (
              <button
                type="button"
                className="msg-action"
                onClick={() => setShowRewind(true)}
                title="Rewind conversation and/or code to this point"
              >
                Rewind
              </button>
            )}
            {onSummarize && !streaming && index > 0 && (
              <button
                type="button"
                className="msg-action"
                onClick={onSummarize}
                title="Replace the thread up to here with a summary, freeing context"
              >
                Summarize from here
              </button>
            )}
            {role === "user" && (
              <>
                <button
                  type="button"
                  className="msg-action msg-action-icon"
                  onClick={editRequest}
                  title="Edit and resend"
                  aria-label="Edit and resend"
                >
                  <EditIcon />
                </button>
                <button
                  type="button"
                  className="msg-action msg-action-icon"
                  onClick={() => void copyResponse()}
                  title={copied ? "Copied" : "Copy"}
                  aria-label={copied ? "Copied" : "Copy"}
                >
                  {copied ? <CheckIcon /> : <CopyIcon />}
                </button>
              </>
            )}
            {role === "assistant" && !streaming && (
              <>
                {onRetry && (
                  <button
                    type="button"
                    className="msg-action"
                    onClick={onRetry}
                    title="Regenerate this answer"
                  >
                    Retry
                  </button>
                )}
                {message.trace && message.trace.length > 0 && (
                  <button type="button" className="msg-action" onClick={() => setShowTrace((s) => !s)} title="Per-step run trace">
                    {showTrace ? "Hide trace" : `Trace · ${message.trace.length}`}
                  </button>
                )}
                <button
                  type="button"
                  className="msg-action msg-action-icon"
                  onClick={() => void copyResponse()}
                  title={copied ? "Copied" : "Copy"}
                  aria-label={copied ? "Copied" : "Copy"}
                >
                  {copied ? <CheckIcon /> : <CopyIcon />}
                </button>
              </>
            )}
          </span>
        </header>
        <div className={`msg-content${role === "assistant" ? " msg-prose" : " msg-user-text"}`}>
          {role === "user" ? (
            content
          ) : segments.length === 0 && content ? (
            <ReactMarkdown remarkPlugins={[remarkGfm]} components={mdComponents}>
              {content}
            </ReactMarkdown>
          ) : role === "assistant" ? (
            <>
              <Segments
                segments={segments}
                streaming={Boolean(streaming)}
                statusByPath={statusByPath}
                projectPath={projectPath}
              />
              {streaming && <span className="msg-caret" aria-hidden />}
            </>
          ) : (
            <span className="msg-error-text">{content}</span>
          )}
        </div>
        {doneStats ? (
          <footer className="msg-stats" title="Run stats">{doneStats}</footer>
        ) : null}
        {changesOpen && changes.length > 0 && (
          <div className="msg-changes" role="list" aria-label="Files changed this turn">
            {changes.map((c) => (
              <div key={c.path} className="msg-changes-row" role="listitem">
                <span className="msg-changes-path" title={c.path}>{c.path || "diff"}</span>
                {c.status && (
                  <span className={`chat-diff-status edit-status--${c.status}`}>{c.status}</span>
                )}
                <span className="msg-changes-stats">
                  <span className="msg-changes-add">+{c.adds}</span>
                  <span className="msg-changes-del">−{c.dels}</span>
                </span>
                <button
                  type="button"
                  className="chat-diff-open"
                  onClick={() =>
                    window.dispatchEvent(
                      new CustomEvent("vencoder-open-editor", {
                        detail:
                          !c.path || /^[A-Za-z]:/.test(c.path) || c.path.startsWith("/")
                            ? c.path
                            : `${projectPath.replace(/[/\\]+$/, "")}/${c.path}`,
                      }),
                    )
                  }
                  title="Open in editor"
                >
                  open
                </button>
              </div>
            ))}
          </div>
        )}
        {showRewind && (
          <RewindMenu
            index={index}
            boundaryTs={(message.ts ?? 0) > 0 ? message.ts! : 0}
            onClose={() => setShowRewind(false)}
            onRewound={(mode, restored) => {
              const what =
                mode === "conversation"
                  ? "conversation"
                  : mode === "code"
                    ? `${restored} file${restored === 1 ? "" : "s"}`
                    : `conversation and ${restored} file${restored === 1 ? "" : "s"}`;
              addActivity(`Rewound ${what} to an earlier turn`, "status");
              setShowRewind(false);
            }}
          />
        )}
        {showTrace && message.trace && message.trace.length > 0 && (
          <div className="msg-trace" role="table" aria-label="Per-step run trace">
            <div className="msg-trace-row msg-trace-head" role="row">
              <span role="columnheader">Step</span>
              <span role="columnheader">Group</span>
              <span role="columnheader">Tools</span>
              <span role="columnheader">Dur</span>
              <span role="columnheader">Tok in</span>
              <span role="columnheader">Tok out</span>
              <span role="columnheader">Outcome</span>
            </div>
            {message.trace.map((t) => (
              <div key={t.stepIndex} className="msg-trace-row" role="row">
                <span role="cell">{t.stepIndex}</span>
                <span role="cell" className="msg-trace-group" title={t.group}>{t.group || "—"}</span>
                <span role="cell">{t.tools.length ? t.tools.join(", ") : "—"}</span>
                <span role="cell">{(t.durationMs / 1000).toFixed(1)}s</span>
                <span role="cell">{t.tokensIn}</span>
                <span role="cell">{t.tokensOut}</span>
                <span role="cell" className={`msg-trace-outcome msg-trace-${t.outcome}`}>{t.outcome}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    </article>
  );
});

function MessageList({
  messages,
  liveTools,
  scrollClassName,
}: {
  messages: ChatMessage[];
  liveTools: Record<string, LiveToolState>;
  scrollClassName?: string;
}) {
  const { pendingEdits, stage } = useAgentStreamContext();
  const { projectPath } = useShell();
  const {
    isProcessing,
    currentConversationId,
    loadConversation,
    bumpHistory,
    addActivity,
    truncateMessages,
    clearMessages,
    addMessage,
  } = useChat();
  // Forking is only meaningful for a named chat (the backend refuses the
  // default/base log) and for turns that were persisted with a turn id.
  const canFork = Boolean(
    currentConversationId && !isDefaultChat(currentConversationId),
  );
  const forkFrom = async (message: ChatMessage) => {
    if (!message.turnId || !currentConversationId) return;
    const { project, chatId } = splitConversationId(currentConversationId);
    const target = newChatId();
    try {
      await api.sessionFork(project, chatId, message.turnId, target);
      await loadConversation(`${project}::${target}`);
      bumpHistory();
    } catch (err) {
      addActivity(
        `Fork failed: ${err instanceof Error ? err.message : String(err)}`,
        "error",
      );
    }
  };
  const [atBottom, setAtBottom] = useState(true);
  const virtuosoRef = useRef<VirtuosoHandle>(null);

  // In-conversation search (Ctrl+F): filter the transcript to turns whose raw
  // text matches, jump between them with Enter / arrows, Esc to clear.
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState("");
  const [searchIdx, setSearchIdx] = useState(0);
  const searchInputRef = useRef<HTMLInputElement>(null);

  const searchMatches = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    if (!q) return [] as number[];
    const hits: number[] = [];
    messages.forEach((m, i) => {
      if ((m.content || "").toLowerCase().includes(q)) hits.push(i);
    });
    return hits;
  }, [messages, searchQuery]);

  const visibleMessages = useMemo(() => {
    if (!searchQuery.trim()) return messages;
    const set = new Set(searchMatches);
    return messages.filter((_, i) => set.has(i));
  }, [messages, searchQuery, searchMatches]);

  const gotoMatch = (dir: 1 | -1) => {
    if (searchMatches.length === 0) return;
    setSearchIdx((i) => {
      const next = (i + dir + searchMatches.length) % searchMatches.length;
      const target = searchMatches[next];
      const visIdx = visibleMessages.findIndex((m) => m === messages[target]);
      if (visIdx >= 0) {
        virtuosoRef.current?.scrollToIndex({ index: visIdx, align: "center", behavior: "smooth" });
      }
      return next;
    });
  };

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && (e.key === "f" || e.key === "F")) {
        e.preventDefault();
        setSearchOpen(true);
        requestAnimationFrame(() => searchInputRef.current?.focus());
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const closeSearch = () => {
    setSearchOpen(false);
    setSearchQuery("");
    setSearchIdx(0);
  };

  // Re-run the most recent user turn at or before `index`: truncate the thread
  // back to that prompt and resend it, so a bad answer can be regenerated
  // without retyping (Cursor/Claude "Retry" parity).
  const retryFrom = (index: number) => {
    let userIdx = -1;
    for (let i = index; i >= 0; i--) {
      if (messages[i]?.role === "user") {
        userIdx = i;
        break;
      }
    }
    if (userIdx < 0) return;
    const prompt = messages[userIdx].content || "";
    if (!prompt.trim()) return;
    truncateMessages(userIdx);
    window.dispatchEvent(new CustomEvent("vencoder-send-prompt", { detail: prompt }));
  };

  // /undo (and double-Esc): revert the last exchange — restore files touched in  // the turn and drop both the assistant reply and its prompt. Mirrors the
  // per-turn RewindMenu "both" path without opening the menu.
  const rewindLastTurn = async () => {
    if (isProcessing) return;
    let userIdx = -1;
    for (let i = messages.length - 1; i >= 0; i--) {
      if (messages[i]?.role === "user") {
        userIdx = i;
        break;
      }
    }
    if (userIdx < 0) return;
    const boundaryTs =
      (messages[messages.length - 1]?.ts ?? 0) || (messages[userIdx]?.ts ?? 0);
    if (!boundaryTs) return;
    try {
      const plan = await planRewind(boundaryTs);
      const restored = await restoreRewind(plan);
      truncateMessages(userIdx);
      addActivity(
        `Undid last turn (${restored} file${restored === 1 ? "" : "s"} restored)`,
        "status",
      );
    } catch (err) {
      addActivity(
        `Undo failed: ${err instanceof Error ? err.message : String(err)}`,
        "error",
      );
    }
  };

  useEffect(() => {
    const onUndo = () => void rewindLastTurn();
    window.addEventListener("vencoder-undo-turn", onUndo);
    let lastEsc = 0;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      const now = Date.now();
      if (now - lastEsc < 500) {
        lastEsc = 0;
        void rewindLastTurn();
      } else {
        lastEsc = now;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("vencoder-undo-turn", onUndo);
      window.removeEventListener("keydown", onKey);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [messages, isProcessing]);

  // Compact the thread up to `index` into a single summary message, dropping
  // the raw turns. Frees context window while preserving intent (Claude
  // "summarize from here" parity); the model sees the digest as history.
  const summarizeFrom = (index: number) => {
    if (isProcessing || index <= 0) return;
    const head = messages.slice(0, index);
    if (head.length === 0) return;
    const lines: string[] = ["[Summary of earlier conversation]"];
    for (const m of head) {
      const text = (m.content || "").replace(/\s+/g, " ").trim();
      if (!text) continue;
      const gist = text.length > 220 ? `${text.slice(0, 220)}…` : text;
      lines.push(`${m.role === "user" ? "User" : "Assistant"}: ${gist}`);
    }
    if (lines.length === 1) return;
    const summary = lines.join("\n");
    const tail = messages.slice(index);
    clearMessages();
    addMessage({ role: "user", content: summary } as ChatMessage);
    for (const m of tail) addMessage(m);
    addActivity("Summarized earlier turns to free context", "status");
  };

  const statusByPath = useMemo(() => {
    const map: Record<string, string> = {};
    for (const group of pendingEdits) {
      for (const edit of group.edits) {
        map[edit.path.replaceAll("\\", "/")] = edit.status;
      }
    }
    return map;
  }, [pendingEdits]);
  const latestMessage = messages[messages.length - 1];
  const messageToolNames = useMemo(() => {
    if (!latestMessage || latestMessage.role !== "assistant") return new Set<string>();
    return new Set(
      parseChatSegments(latestMessage.content || "")
        .filter((s) => s.kind === "tool")
        .map((s) => s.name),
    );
  }, [latestMessage]);
  const liveToolList = Object.values(liveTools).filter((lt) => !messageToolNames.has(lt.tool));
  const activeStage = stageOrQueued(stage, isProcessing);
  const showStage = isStageVisible(activeStage);

  const jumpLatest = () => {
    virtuosoRef.current?.scrollToIndex({
      index: messages.length - 1,
      align: "end",
      behavior: "smooth",
    });
  };

  const LiveFooter = liveToolList.length > 0 || showStage ? () => (
    <article className="msg-turn msg-turn--assistant is-streaming">
      <div className="msg-avatar" aria-hidden>
        <span className="msg-avatar-icon msg-avatar-ai">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" />
          </svg>
        </span>
      </div>
      <div className="msg-body">
        <header className="msg-meta">
          <span className="msg-role">{showStage ? "Working" : "Tools"}</span>
        </header>
        {showStage && <AgentStageIndicator stage={activeStage} />}
        <div className="msg-content msg-prose">
          {liveToolList.map((lt) => (
            <ToolExecutionBadge
              key={lt.id}
              name={lt.tool}
              status={(lt.status === "done" ? "success" : lt.status) as ToolStatus}
              output={lt.output ?? lt.summary ?? ""}
              streaming={lt.status === "running"}
              durationMs={lt.durationMs}
              exitCode={lt.exitCode}
            />
          ))}
        </div>
      </div>
    </article>
  ) : undefined;

  const MessageItem = ({ children }: { children?: ReactNode }) => (
    <div className="messages">{children}</div>
  );

  return (
    <div
      className="messages-list"
      role="log"
      aria-live="polite"
      aria-relevant="additions text"
      aria-label="Conversation"
    >
      {searchOpen && (
        <div className="chat-search" role="search">
          <input
            ref={searchInputRef}
            className="chat-search-input"
            value={searchQuery}
            placeholder="Search this conversation…"
            aria-label="Search conversation"
            onChange={(e) => {
              setSearchQuery(e.target.value);
              setSearchIdx(0);
            }}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.preventDefault();
                closeSearch();
              } else if (e.key === "Enter") {
                e.preventDefault();
                gotoMatch(e.shiftKey ? -1 : 1);
              }
            }}
          />
          <span className="chat-search-count">
            {searchQuery.trim()
              ? `${searchMatches.length ? searchIdx + 1 : 0}/${searchMatches.length}`
              : ""}
          </span>
          <button type="button" className="chat-search-nav" onClick={() => gotoMatch(-1)} title="Previous match">
            ↑
          </button>
          <button type="button" className="chat-search-nav" onClick={() => gotoMatch(1)} title="Next match">
            ↓
          </button>
          <button type="button" className="chat-search-close" onClick={closeSearch} title="Close (Esc)">
            ✕
          </button>
        </div>
      )}
      <Virtuoso
        ref={virtuosoRef}
        className={scrollClassName}
        style={{ height: "100%", flex: 1, minHeight: 0 }}
        data={visibleMessages}
        computeItemKey={(index) => visibleMessages[index]?.turnId ?? index}
        itemContent={(index, message) => {
          const original = messages.indexOf(message);
          return (
            <Message
              message={message}
              index={original}
              statusByPath={statusByPath}
              projectPath={projectPath}
              onFork={
                canFork && message.turnId && !message.streaming
                  ? () => void forkFrom(message)
                  : undefined
              }
              onRetry={
                message.role === "assistant" && !message.streaming && original > 0
                  ? () => retryFrom(original)
                  : undefined
              }
              onSummarize={
                !message.streaming && original === messages.length - 1 && original > 0
                  ? () => summarizeFrom(original)
                  : undefined
              }
            />
          );
        }}
        components={{ Item: MessageItem, Footer: LiveFooter }}
        followOutput={(isAtBottom) => (isAtBottom ? "smooth" : false)}
        atBottomStateChange={setAtBottom}
        increaseViewportBy={{ top: 320, bottom: 320 }}
      />
      {!atBottom && messages.length > 0 && (
        <button
          type="button"
          className="chat-scroll-down"
          onClick={jumpLatest}
          title="Jump to latest"
        >
          ↓ latest
        </button>
      )}
    </div>
  );
}

export default MessageList;