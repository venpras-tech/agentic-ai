import { useEffect, useRef, useState } from "react";
import { useApp } from "../context/AppContext";
import { api } from "../lib/ipc";
import FileTreePanel from "../components/editor/FileTreePanel";
import EditorPane from "../components/editor/EditorPane";
import MessageList from "../components/MessageList";
import ActivityPanel from "../components/ActivityPanel";
import HitlGateway from "../components/chat/HitlGateway";
import AgentTimeline from "../components/chat/AgentTimeline";
import { PermissionPrompt, QuestionPrompt } from "../components/chat/PermissionPrompt";
import ChatComposer from "../components/ChatComposer";
import { useAgentStream } from "../hooks/useAgentStream";
import "./WorkspacePage.css";

function WorkspacePage() {
  const { messages, activityLog, addContextPath, projectPath, setProjectPath } = useApp();
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
  const [selected, setSelected] = useState<string | null>(null);
  const [activityCollapsed, setActivityCollapsed] = useState(false);
  const [stickBottom, setStickBottom] = useState(true);
  const [treeW, setTreeW] = useState(240);
  const [chatW, setChatW] = useState(400);
  const [dragging, setDragging] = useState<null | "tree" | "chat">(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && stickBottom) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages, liveTools, stickBottom]);

  const handleScroll = () => {
    const el = scrollRef.current;
    if (!el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    setStickBottom(nearBottom);
  };

  const handleBrowse = async () => {
    try {
      const path = await api.pickWorkspaceFolder();
      if (path) {
        setProjectPath(path);
        await api.agentSetWorkspace(path);
      }
    } catch {
      // dialog dismissed
    }
  };

  const startDrag = (which: "tree" | "chat") => (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const startX = e.clientX;
    const startW = which === "tree" ? treeW : chatW;
    const setW = which === "tree" ? setTreeW : setChatW;
    const clampW = (v: number) =>
      which === "tree" ? Math.min(520, Math.max(160, v)) : Math.min(640, Math.max(260, v));
    const onMove = (ev: PointerEvent) => {
      const dx = ev.clientX - startX;
      setW(clampW(which === "tree" ? startW + dx : startW - dx));
    };
    const onUp = () => {
      setDragging(null);
      document.body.style.userSelect = "";
      document.body.style.cursor = "";
      window.removeEventListener("pointermove", onMove);
    };
    setDragging(which);
    document.body.style.userSelect = "none";
    document.body.style.cursor = "col-resize";
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp, { once: true });
  };

  return (
    <div
      className="workspace-view"
      style={{ gridTemplateColumns: `${treeW}px 6px minmax(0, 1fr) 6px ${chatW}px` }}
    >
      <div className="workspace-pane workspace-tree">
        <FileTreePanel projectPath={projectPath} selectedPath={selected} onSelect={setSelected} />
      </div>
      <div
        className={`workspace-divider${dragging === "tree" ? " dragging" : ""}`}
        onPointerDown={startDrag("tree")}
        title="Drag to resize"
      />
      <div className="workspace-pane workspace-editor">
        <EditorPane
          selectedPath={selected}
          projectPath={projectPath}
          onAddContext={(rel) => addContextPath(rel)}
        />
      </div>
      <div
        className={`workspace-divider${dragging === "chat" ? " dragging" : ""}`}
        onPointerDown={startDrag("chat")}
        title="Drag to resize"
      />
      <div className="workspace-pane workspace-chat">
        <div className="workspace-chat-header">
          <span>Chat</span>
          <button type="button" className="btn-index" onClick={() => void handleBrowse()} title="Change project folder">
            {projectPath ? projectPath.split(/[/\\]/).pop() : "Set workspace"}
          </button>
        </div>
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
        <div className="workspace-chat-scroll" ref={scrollRef} onScroll={handleScroll}>
          {messages.length === 0 ? (
            <div className="workspace-chat-empty">Ask the agent to explore or edit code — changes appear in the editor pane.</div>
          ) : (
            <MessageList messages={messages} liveTools={liveTools} />
          )}
          {!stickBottom && messages.length > 0 && (
            <button
              type="button"
              className="chat-scroll-down"
              onClick={() => {
                const el = scrollRef.current;
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
        <div className="workspace-composer">
          <ChatComposer variant="bottom" />
        </div>
      </div>
    </div>
  );
}

export default WorkspacePage;