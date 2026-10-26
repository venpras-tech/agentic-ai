import { useEffect, useMemo, useRef } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { ChatMessage } from "../../context/types";
import { stripChatTags } from "../../lib/chatParse";
import "./ChatThread.css";

function displayText(message: ChatMessage): string {
  const raw = message.content || "";
  if (message.role !== "assistant") return raw;
  const stripped = stripChatTags(raw).trim();
  return stripped || raw.trim();
}

function ChatThread({
  messages,
  isProcessing,
}: {
  messages: ChatMessage[];
  isProcessing: boolean;
}) {
  const endRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const last = messages[messages.length - 1];
  const waitingForReply =
    isProcessing && last?.role === "user" && !messages.some((m) => m.role === "assistant" && m.streaming);

  useEffect(() => {
    endRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [messages, isProcessing, last?.content]);

  const rows = useMemo(() => messages, [messages]);

  return (
    <div className="chat-thread" ref={scrollRef} role="log" aria-live="polite" aria-label="Chat messages">
      {rows.length === 0 ? (
        <p className="chat-thread-empty">No messages yet — send a prompt below.</p>
      ) : null}
      {rows.map((m, i) => {
        const text = displayText(m);
        const showThinking = m.role === "assistant" && m.streaming && !text;
        return (
          <article
            key={m.turnId ?? `msg-${i}-${m.ts ?? 0}`}
            className={`chat-thread-turn chat-thread-turn--${m.role}${m.streaming ? " is-streaming" : ""}`}
          >
            <header className="chat-thread-meta">
              <span className="chat-thread-role">{m.role === "user" ? "You" : m.model?.name || "Assistant"}</span>
              {m.ts ? (
                <time className="chat-thread-time">
                  {new Date(m.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
                </time>
              ) : null}
            </header>
            <div className="chat-thread-body">
              {m.role === "user" ? (
                <p className="chat-thread-user">{text}</p>
              ) : showThinking ? (
                <p className="chat-thread-thinking">
                  <span className="chat-thread-dots" aria-hidden />
                  Responding…
                </p>
              ) : text ? (
                <div className="chat-thread-prose">
                  <ReactMarkdown remarkPlugins={[remarkGfm]}>{text}</ReactMarkdown>
                </div>
              ) : !m.streaming ? (
                <p className="chat-thread-muted">No content in this reply.</p>
              ) : null}
              {m.streaming && text ? <span className="chat-thread-caret" aria-hidden /> : null}
            </div>
            {m.done && !m.streaming ? (
              <footer className="chat-thread-stats">
                {[
                  m.done.outputTokens != null ? `${m.done.outputTokens} tok` : null,
                  m.done.elapsedMs ? `${(m.done.elapsedMs / 1000).toFixed(1)}s` : null,
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </footer>
            ) : null}
          </article>
        );
      })}
      {waitingForReply ? (
        <article className="chat-thread-turn chat-thread-turn--assistant is-streaming">
          <header className="chat-thread-meta">
            <span className="chat-thread-role">Assistant</span>
          </header>
          <p className="chat-thread-thinking">
            <span className="chat-thread-dots" aria-hidden />
            Starting…
          </p>
        </article>
      ) : null}
      <div ref={endRef} className="chat-thread-anchor" />
    </div>
  );
}

export default ChatThread;
