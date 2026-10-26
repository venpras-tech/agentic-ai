import type { ReactNode } from "react";
import { useChat } from "../../context/AppContext";
import ChatThread from "./ChatThread";
import "./ChatView.css";

interface ChatViewProps {
  header?: ReactNode;
  emptyState?: ReactNode;
}

function ChatView({ header, emptyState }: ChatViewProps) {
  const { messages, isProcessing } = useChat();
  const showWelcome = messages.length === 0 && emptyState;

  return (
    <div className="chat-view">
      {header}
      <div className="chat-view-main">
        {showWelcome ? (
          <div className="chat-view-welcome">{emptyState}</div>
        ) : (
          <ChatThread messages={messages} isProcessing={isProcessing} />
        )}
      </div>
    </div>
  );
}

export default ChatView;
