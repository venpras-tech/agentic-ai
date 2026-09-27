import { useEffect, useRef, useState } from "react";
import { useApp } from "../context/AppContext";
import MessageList from "../components/MessageList";
import ActivityPanel from "../components/ActivityPanel";
import HitlGateway from "../components/chat/HitlGateway";
import AgentTimeline from "../components/chat/AgentTimeline";
import { PermissionPrompt, QuestionPrompt } from "../components/chat/PermissionPrompt";
import { useAgentStream } from "../hooks/useAgentStream";
import "./ChatPage.css";

const welcomeSuggestions = [
  { label: "Add a login form", prompt: "Add a login form to the app" },
  { label: "Find auth logic", prompt: "Find where authentication is handled" },
  { label: "Run tests & fix", prompt: "Run the test suite and fix any failures" },
];

function ChatPage() {
  const { messages, activityLog, modelName, modelProvider, modelLoading, modelLoadProgress,
    modelLoadStage } = useApp();
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
  }, [messages, stickBottom]);

  const handleContainerScroll = () => {
    const el = containerRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setStickBottom(nearBottom);
  };

  const applySuggestion = (prompt: string) => {
    window.dispatchEvent(new CustomEvent("vencoder-prefill-composer", { detail: prompt }));
  };

  return (
    <div className="chat-content">
      {hitlGate && (
        <HitlGateway
          reason={hitlGate.reason}
          onApprove={() => void resolveGate(true)}
          onDeny={() => void resolveGate(false)}
        />
      )}
      {permission && <PermissionPrompt request={permission} onDecide={(d) => void respondPermission(d)} />}
      {question && <QuestionPrompt request={question} onAnswer={(a) => void respondQuestion(a)} />}
      {(modelLoading || modelName) && (
        <div className={`chat-model-banner${modelLoading ? " loading" : ""}`}>
          {modelLoading ? (
            <>
              <span className="chat-model-spinner" />
              <span className="chat-model-banner-text">
                {modelLoadProgress != null ? `Loading model… ${Math.round(modelLoadProgress * 100)}%` : `Loading model (${modelLoadStage || "init"})…`}
              </span>
              {modelLoadProgress != null && (
                <span className="chat-model-track"><span className="chat-model-fill" style={{ width: `${Math.min(100, Math.max(0, Math.round(modelLoadProgress * 100)))}%` }} /></span>
              )}
            </>
          ) : (
            <>
              <span className="chat-model-dot" />
              <span className="chat-model-banner-text">{modelName || "Model"}<span className="chat-model-provider">{modelProvider ? ` · ${modelProvider}` : ""}</span></span>
            </>
          )}
        </div>
      )}
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
      <div className="messages-container" ref={containerRef} onScroll={handleContainerScroll}>
        {messages.length === 0 ? (
          <div className="chat-welcome">
            <h2 className="chat-welcome-title">What can I build?</h2>
            <p className="chat-welcome-text">
              {modelName
                ? `${modelName} is ready to write, debug, refactor, or explain your code.`
                : "Load a model to get started"}
            </p>
            <div className="chat-welcome-suggestions">
              {welcomeSuggestions.map(({ label, prompt }) => (
                <button
                  key={prompt}
                  type="button"
                  className="chat-suggestion"
                  onClick={() => applySuggestion(prompt)}
                >
                  {label}
                </button>
              ))}
            </div>
            <div className="chat-welcome-hints">
              <kbd className="chat-kbd">Enter</kbd> send ·{" "}
              <kbd className="chat-kbd">Shift+Enter</kbd> newline ·{" "}
              <kbd className="chat-kbd">/</kbd> commands
            </div>
          </div>
        ) : (
          <MessageList messages={messages} liveTools={liveTools} />
        )}
        {!stickBottom && messages.length > 0 && (
          <button
            type="button"
            className="chat-scroll-down"
            onClick={() => {
              const el = containerRef.current;
              if (el) {
                el.scrollTop = el.scrollHeight;
                setStickBottom(true);
              }
            }}
            title="Jump to latest"
          >
            ↓ latest
          </button>
        )}
      </div>
    </div>
  );
}

export default ChatPage;