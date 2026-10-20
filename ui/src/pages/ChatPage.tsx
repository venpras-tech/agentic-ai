import { useApp } from "../context/AppContext";
import AgentChatSurface from "../components/chat/AgentChatSurface";
import "./ChatPage.css";

const welcomeSuggestions = [
  { label: "Add a login form", prompt: "Add a login form to the app" },
  { label: "Find auth logic", prompt: "Find where authentication is handled" },
  { label: "Run tests & fix", prompt: "Run the test suite and fix any failures" },
];

function ChatPage() {
  const { modelName, modelProvider, modelLoading, modelLoadProgress, modelLoadStage } = useApp();

  const modelBanner =
    (modelLoading || modelName) && (
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
    );

  const welcome = (
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
            onClick={() =>
              window.dispatchEvent(new CustomEvent("vencoder-prefill-composer", { detail: prompt }))
            }
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
  );

  return (
    <div className="chat-content">
      <AgentChatSurface
        scrollClassName="messages-container"
        emptyState={welcome}
        header={modelBanner}
      />
    </div>
  );
}

export default ChatPage;
