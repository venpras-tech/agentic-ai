import { useEffect, useRef, useState, type ReactNode } from "react";
import { useApp } from "../../context/AppContext";
import { useAgentStream } from "../../context/AgentStreamContext";
import MessageList from "../MessageList";
import ActivityPanel from "../ActivityPanel";
import HitlGateway from "./HitlGateway";
import AgentTimeline from "./AgentTimeline";
import { PermissionPrompt, QuestionPrompt } from "./PermissionPrompt";

export interface AgentChatSurfaceProps {
  /** Layout-specific class for the scroll container (`messages-container`, `workspace-chat-scroll`). */
  scrollClassName: string;
  /** Rendered in place of the message list while the chat is empty. */
  emptyState?: ReactNode;
  /** Rendered above the HITL/permission row (model banner, chat header, …). */
  header?: ReactNode;
}

/**
 * The chat surface shared by `/chat` and `/workspace`: HITL prompts, the
 * activity log, the agent timeline, the scrolling message list and the
 * "↓ latest" affordance. Pages supply only their layout-specific wrappers.
 *
 * All stream state comes from `AgentStreamProvider`, so this component (and the
 * page hosting it) can mount and unmount mid-run without disturbing a run.
 */
export default function AgentChatSurface({
  scrollClassName,
  emptyState,
  header,
}: AgentChatSurfaceProps) {
  const { messages, activityLog } = useApp();
  const {
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
  } = useAgentStream();
  const [activityCollapsed, setActivityCollapsed] = useState(false);
  const [stickBottom, setStickBottom] = useState(true);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = containerRef.current;
    if (el && stickBottom) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages, liveTools, stickBottom]);

  const handleScroll = () => {
    const el = containerRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setStickBottom(nearBottom);
  };

  const jumpToLatest = () => {
    const el = containerRef.current;
    if (el) {
      el.scrollTop = el.scrollHeight;
      setStickBottom(true);
    }
  };

  return (
    <>
      {header}
      {hitlGate && (
        <HitlGateway
          reason={hitlGate.reason}
          onApprove={() => void resolveGate(true)}
          onDeny={() => void resolveGate(false)}
        />
      )}
      {permission && <PermissionPrompt request={permission} onDecide={(d) => void respondPermission(d)} />}
      {question && <QuestionPrompt request={question} onAnswer={(a) => void respondQuestion(a)} />}
      <ActivityPanel
        logs={activityLog}
        collapsed={activityCollapsed}
        onToggle={() => setActivityCollapsed(!activityCollapsed)}
      />
      <AgentTimeline
        planSteps={planSteps}
        subtasks={subtasks}
        todos={todos}
        bgTasks={bgTasks}
        onAbortBackground={(id) => void abortBackgroundTask(id)}
      />
      <div className={scrollClassName} ref={containerRef} onScroll={handleScroll}>
        {messages.length === 0 ? (
          emptyState ?? null
        ) : (
          <MessageList messages={messages} liveTools={liveTools} />
        )}
        {!stickBottom && messages.length > 0 && (
          <button
            type="button"
            className="chat-scroll-down"
            onClick={jumpToLatest}
            title="Jump to latest"
          >
            ↓ latest
          </button>
        )}
      </div>
    </>
  );
}
