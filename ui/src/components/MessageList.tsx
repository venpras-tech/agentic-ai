import { memo, type ReactNode, useMemo, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { parseChatSegments } from "../lib/chatParse";
import type { ChatSegment } from "../lib/chatParse";
import { messageKey } from "../lib/messageKey";
import type { ChatMessage, LiveToolState } from "../context/types";
import ReasoningAccordion from "./chat/ReasoningAccordion";
import ToolExecutionBadge from "./chat/ToolExecutionBadge";
import type { ToolStatus } from "./chat/ToolExecutionBadge";
import DiffViewer from "./chat/DiffViewer";
import CodeBlock from "./chat/CodeBlock";
import "./MessageList.css";

const markdownComponents = {
  code: (props: {
    node?: unknown;
    className?: string;
    children?: ReactNode;
    inline?: boolean;
  }) => {
    if (props.className === "language-diff") {
      return <DiffViewer content={String(props.children ?? "")} />;
    }
    return <CodeBlock {...props} />;
  },
  p: ({ children }: { children?: ReactNode }) => <p className="msg-p">{children}</p>,
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

function SegmentView({
  segment,
  isLast,
  streaming,
}: {
  segment: ChatSegment;
  isLast: boolean;
  streaming: boolean;
}) {
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
        />
      );
    case "diff":
      return <DiffViewer path={segment.path} content={segment.text} />;
    case "gate":
      return (
        <div className="chat-gate-marker">
          <span className="chat-gate-chip">⛔ approval requested</span>
        </div>
      );
    case "text":
      if (!segment.text.trim()) return null;
      return (
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
          {segment.text}
        </ReactMarkdown>
      );
    default:
      return null;
  }
}

const Message = memo(function Message({
  message,
  index,
  segments,
}: {
  message: ChatMessage;
  index: number;
  /** Pre-parsed assistant segments, memoized by the list (see parse cache). */
  segments: ChatSegment[];
}) {
  const { role, content, streaming } = message;
  const [copied, setCopied] = useState(false);

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

  const roleLabel =
    role === "user"
      ? "You"
      : message.model
        ? message.model.provider === "Local"
          ? message.model.name
          : message.model.name
        : "Assistant";

  const lastRealIndex = segments.reduce((acc, s, i) => (s.kind !== "gate" ? i : acc), -1);

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
          <span className="msg-actions">
            {role === "user" && (
              <>
                <button type="button" className="msg-action" onClick={editRequest} title="Edit and resend">
                  Edit
                </button>
                <button type="button" className="msg-action" onClick={() => void copyResponse()} title="Copy">
                  {copied ? "Copied" : "Copy"}
                </button>
              </>
            )}
            {role === "assistant" && !streaming && (
              <button type="button" className="msg-action" onClick={() => void copyResponse()} title="Copy">
                {copied ? "Copied" : "Copy"}
              </button>
            )}
          </span>
        </header>
        <div className={`msg-content${role === "assistant" ? " msg-prose" : " msg-user-text"}`}>
          {role === "user" ? (
            content
          ) : segments.length === 0 && content ? (
            <ReactMarkdown remarkPlugins={[remarkGfm]} components={markdownComponents}>
              {content}
            </ReactMarkdown>
          ) : role === "assistant" ? (
            <>
              {segments.map((seg, i) => (
                <SegmentView
                  key={`${seg.kind}-${i}`}
                  segment={seg}
                  isLast={i === lastRealIndex}
                  streaming={Boolean(streaming)}
                />
              ))}
              {streaming && <span className="msg-caret" aria-hidden />}
            </>
          ) : (
            <span className="msg-error-text">{content}</span>
          )}
        </div>
        {doneStats ? (
          <footer className="msg-stats" title="Run stats">{doneStats}</footer>
        ) : null}
      </div>
    </article>
  );
});

/**
 * Parse-cache keyed by message key. `parseChatSegments` walks the whole
 * transcript with a global regex, so re-parsing every message on each streamed
 * token is what makes long chats stutter; only the streaming tail actually
 * changes, and its `content` guard forces a re-parse of that one entry.
 */
function useSegmentCache() {
  const cacheRef = useRef(new Map<string, { content: string; segments: ChatSegment[] }>());
  return (key: string, message: ChatMessage): ChatSegment[] => {
    if (message.role !== "assistant") return [];
    const content = message.content || "";
    const hit = cacheRef.current.get(key);
    if (hit && hit.content === content) return hit.segments;
    const segments = parseChatSegments(content);
    cacheRef.current.set(key, { content, segments });
    return segments;
  };
}

function MessageList({
  messages,
  liveTools,
}: {
  messages: ChatMessage[];
  liveTools: Record<string, LiveToolState>;
}) {
  const segmentsFor = useSegmentCache();

  // A tool the stream already emitted as a `<tool_call>` segment is rendered
  // inside the assistant bubble; showing its live badge too would double it.
  const lastMessage = messages[messages.length - 1];
  const streamingToolNames = new Set(
    lastMessage?.streaming && lastMessage.role === "assistant"
      ? parseChatSegments(lastMessage.content || "")
          .filter((s): s is Extract<ChatSegment, { kind: "tool" }> => s.kind === "tool")
          .map((s) => s.name)
      : [],
  );
  const liveToolList = Object.values(liveTools).filter((lt) => !streamingToolNames.has(lt.tool));

  const rows = useMemo(
    () =>
      messages.map((message, index) => {
        const key = messageKey(message, index);
        return {
          key,
          message,
          index,
          segments: segmentsFor(key, message),
        };
      }),
    // `segmentsFor` is a stable ref-backed callback; `messages` identity is the
    // only thing that can change the parsed rows.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [messages],
  );

  return (
    <div className="messages">
      {rows.map(({ key, message, index, segments }) => (
        <Message key={key} message={message} index={index} segments={segments} />
      ))}
      {liveToolList.length > 0 && (
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
              <span className="msg-role">Working</span>
            </header>
            <div className="msg-content msg-prose">
              {liveToolList.map((lt) => (
                <ToolExecutionBadge
                  key={lt.id}
                  name={lt.tool}
                  status={(lt.status === "done" ? "success" : lt.status) as ToolStatus}
                  output={lt.output ?? lt.summary ?? ""}
                  streaming={lt.status === "running"}
                />
              ))}
            </div>
          </div>
        </article>
      )}
    </div>
  );
}

export default MessageList;