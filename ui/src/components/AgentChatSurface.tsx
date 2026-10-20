import { useState, type ReactNode } from "react";
import { useChat } from "../context/AppContext";
import { useAgentStreamContext } from "../context/AgentStreamContext";
import ActivityPanel from "./ActivityPanel";
import HitlGateway from "./chat/HitlGateway";
import AgentTimeline from "./chat/AgentTimeline";
import ExecutionGraphPanel from "./chat/ExecutionGraphPanel";
import { PermissionPrompt, QuestionPrompt } from "./chat/PermissionPrompt";
import EditReviewPanel from "./chat/EditReviewPanel";
import TaskChecklist from "./chat/TaskChecklist";
import RunBoard from "./chat/RunBoard";
import MessageList, { type ChatMessage, type LiveToolState } from "./MessageList";

interface AgentChatSurfaceProps {
  messages: ChatMessage[];
  liveTools: Record<string, LiveToolState>;
  /** Scroll container class — the surface owns stick-to-bottom + "latest". */
  scrollClassName: string;
  emptyState: ReactNode;
  /** Page-specific banners rendered above the activity/timeline blocks. */
  children?: ReactNode;
}

/**
 * Shared layout for the agent chat: HITL gate, permission/question prompts,
 * activity log, timeline, the pending-edit review panel and the scrolling
 * message list with stick-to-bottom behaviour. ChatPage and WorkspacePage are
 * thin shells over this so the mid-run behaviour is identical on both routes.
 */
function AgentChatSurface({
  messages,
  liveTools,
  scrollClassName,
  emptyState,
  children,
}: AgentChatSurfaceProps) {
  const { activityLog, isProcessing } = useChat();
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
    execGraph,
    clearExecGraph,
  } = useAgentStreamContext();
  const [activityCollapsed, setActivityCollapsed] = useState(false);
  const [activityFilter, setActivityFilter] = useState("");
  const [graphOpen, setGraphOpen] = useState(true);
  const hasGraph = execGraph.rootId != null;
  const [boardOpen, setBoardOpen] = useState(false);
  const hasRuns = subagentRuns.length > 0 || bgTasks.length > 0;

  const openRunLog = (query: string) => {
    setActivityFilter(query);
    setActivityCollapsed(false);
  };

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
      <ActivityPanel
        logs={activityLog}
        collapsed={activityCollapsed}
        onToggle={() => setActivityCollapsed(!activityCollapsed)}
        filter={activityFilter}
        onClearFilter={() => setActivityFilter("")}
      />
      <AgentTimeline
        planSteps={planSteps}
        subtasks={subtasks}
        subagentRuns={subagentRuns}
        todos={todos}
        bgTasks={bgTasks}
        onAbortBackground={(id) => void abortBackgroundTask(id)}
        onViewLog={openRunLog}
      />
      <EditReviewPanel />
      {hasRuns && boardOpen && (
        <RunBoard
          subagentRuns={subagentRuns}
          bgTasks={bgTasks}
          onAbortBackground={(id) => void abortBackgroundTask(id)}
          onClose={() => setBoardOpen(false)}
        />
      )}
      {hasRuns && !boardOpen && (
        <button
          type="button"
          className="run-board-reopen"
          onClick={() => setBoardOpen(true)}
          title="Show the multiplexed run board"
        >
          Runs · {subagentRuns.length + bgTasks.length}
        </button>
      )}
      <div className={`agent-chat-messages ${scrollClassName}`}>
        {messages.length === 0 ? emptyState : (
          <>
            {(isProcessing || todos.some((t) => !t.done)) && todos.length > 0 && (
              <TaskChecklist items={todos} />
            )}
            <MessageList messages={messages} liveTools={liveTools} scrollClassName="agent-chat-messages-inner" />
          </>
        )}
      </div>
      {hasGraph && graphOpen && (
        <ExecutionGraphPanel
          state={execGraph}
          visible
          onClear={clearExecGraph}
          onClose={() => setGraphOpen(false)}
        />
      )}
      {hasGraph && !graphOpen && (
        <button
          type="button"
          className="execgraph-reopen"
          onClick={() => setGraphOpen(true)}
          title="Show execution graph"
        >
          Execution graph · {execGraph.nodes.size}
        </button>
      )}
    </div>
  );
}

export default AgentChatSurface;