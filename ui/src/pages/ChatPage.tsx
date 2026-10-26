import { useCallback, useMemo, useRef, useState } from "react";
import { useChat, useModel } from "../context/AppContext";
import AgentChatSurface from "../components/AgentChatSurface";
import AgentBackendPicker from "../components/AgentBackendPicker";
import { useAgentStreamContext } from "../context/AgentStreamContext";
import { exportConversation, ExportError, type ExportFormat } from "../lib/exportChat";
import "./ChatPage.css";

function ChatPage() {
  const { messages } = useChat();
  const { modelName, modelProvider, modelLoading, modelLoadProgress, modelLoadStage } = useModel();
  const { liveTools } = useAgentStreamContext();
  const [exportFormat, setExportFormat] = useState<ExportFormat>("pdf");
  const [exportToast, setExportToast] = useState<{ text: string; error?: boolean } | null>(null);
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const notify = useCallback((text: string, error?: boolean) => {
    setExportToast({ text, error });
    if (toastTimerRef.current) clearTimeout(toastTimerRef.current);
    toastTimerRef.current = setTimeout(() => setExportToast(null), 3200);
  }, []);

  const exportChat = useCallback(async () => {
    if (messages.length === 0) return;
    const firstUser = messages.find((m) => m.role === "user");
    const title = firstUser ? firstUser.content.slice(0, 60).replace(/\n/g, " ") : "Chat Export";
    try {
      const filename = await exportConversation({ messages, title, format: exportFormat });
      notify(`Exported ${filename}`);
    } catch (err) {
      notify(err instanceof ExportError ? err.message : `Export failed: ${String(err)}`, true);
    }
  }, [messages, exportFormat, notify]);

  const hasModel = modelLoading || Boolean(modelName);
  const showBar = hasModel || messages.length > 0;

  const modelBanner = showBar && (
    <div className={`chat-model-banner${modelLoading ? " loading" : ""}`}>
      {modelLoading ? (
        <>
          <span className="chat-model-spinner" />
          <span className="chat-model-banner-text">
            {modelLoadProgress != null
              ? `Loading model… ${Math.round(modelLoadProgress * 100)}%`
              : `Loading model (${modelLoadStage || "init"})…`}
          </span>
          {modelLoadProgress != null && (
            <span className="chat-model-track">
              <span
                className="chat-model-fill"
                style={{
                  width: `${Math.min(100, Math.max(0, Math.round(modelLoadProgress * 100)))}%`,
                }}
              />
            </span>
          )}
        </>
      ) : modelName ? (
        <>
          <span className="chat-model-dot" />
          <span className="chat-model-banner-text">
            {modelName}
            <span className="chat-model-provider">
              {modelProvider ? ` · ${modelProvider}` : ""}
            </span>
          </span>
        </>
      ) : (
        <span className="chat-model-banner-text" />
      )}
      <AgentBackendPicker compact />
      {messages.length > 0 && (
        <div className="chat-export-controls">
          {exportToast && (
            <span
              className={`chat-export-toast${exportToast.error ? " error" : ""}`}
              role="status"
            >
              {exportToast.text}
            </span>
          )}
          <select
            value={exportFormat}
            onChange={(e) => setExportFormat(e.target.value as ExportFormat)}
            className="chat-export-select"
            aria-label="Export format"
            title="Export format"
          >
            <option value="pdf">PDF</option>
            <option value="docx">DOCX</option>
            <option value="csv">CSV</option>
          </select>
          <button
            type="button"
            className="chat-export-btn"
            onClick={() => void exportChat()}
            title={`Export chat as ${exportFormat.toUpperCase()}`}
            aria-label={`Export chat as ${exportFormat.toUpperCase()}`}
          >
            <span aria-hidden="true">⤓</span> export
          </button>
        </div>
      )}
    </div>
  );

  return (
    <div className="chat-content">
      <AgentChatSurface
        messages={messages}
        liveTools={liveTools}
        scrollClassName="messages-container"
        emptyState={
          <div className="chat-welcome">
            <h1 className="chat-welcome-brand">Vencoder</h1>
            <p className="chat-welcome-tagline">
              Your personal AI assistant. What do we do today?
            </p>
            {!modelName && !modelLoading && (
              <p className="chat-welcome-hint">Connect a model under Agents to get started.</p>
            )}
          </div>
        }
      >
        {modelBanner}
      </AgentChatSurface>
    </div>
  );
}

export default ChatPage;
