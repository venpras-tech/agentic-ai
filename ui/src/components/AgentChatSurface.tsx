import { type ReactNode } from "react";
import { useChat } from "../context/AppContext";
import { useAgentStreamContext } from "../context/AgentStreamContext";
import HitlGateway from "./chat/HitlGateway";
import AgentTimeline from "./chat/AgentTimeline";
import { PermissionPrompt, QuestionPrompt } from "./chat/PermissionPrompt";
import EditReviewPanel from "./chat/EditReviewPanel";
import TaskChecklist from "./chat/TaskChecklist";
import MessageList, { type ChatMessage, type LiveToolState } from "./MessageList";

interface AgentChatSurfaceProps {
  messages: ChatMessage[];
  liveTools: Record<string, LiveToolState>;
  scrollClassName: string;
  emptyState: ReactNode;
  children?: ReactNode;
}

function AgentChatSurface({
  messages,
  liveTools,
  scrollClassName,
  emptyState,
  children,
}: AgentChatSurfaceProps) {
  const { isProcessing } = useChat();
  const {
    hitlGate,
    resolveGate,
    permission,
    respondPermission,
    question,
    respondQuestion,
    planSteps,
    todos,
    subtasks,
    subagentRuns,
    bgTasks,
    abortBackgroundTask,
  } = useAgentStreamContext();

  return (
    <div className="agent-chat-surface">
      {hitlGate && (
        <HitlGateway
          reason={hitlGate.reason}
          onApprove={() => void resolveGate(true)}
          onDeny={() => void resolveGate(false)}
        />
      )}
      {permission && <PermissionPrompt request={permission} onDecide={(d) => void respondPermission(d)} />}
      {question && <QuestionPrompt request={question} onAnswer={(a) => void respondQuestion(a)} />}
      {children}
      <AgentTimeline
        planSteps={planSteps}
        subtasks={subtasks}
        subagentRuns={subagentRuns}
        todos={todos}
        bgTasks={bgTasks}
        onAbortBackground={(id) => void abortBackgroundTask(id)}
        onViewLog={() => {}}
      />
      <EditReviewPanel />
      <div className={`agent-chat-messages ${scrollClassName}`}>
        {messages.length === 0 ? (
          emptyState
        ) : (
          <div className="agent-chat-messages-viewport">
            {(isProcessing || todos.some((t) => !t.done)) && todos.length > 0 && (
              <TaskChecklist items={todos} />
            )}
            <MessageList messages={messages} liveTools={liveTools} scrollClassName="agent-chat-messages-inner" />
          </div>
        )}
      </div>
    </div>
  );
}

export default AgentChatSurface;
