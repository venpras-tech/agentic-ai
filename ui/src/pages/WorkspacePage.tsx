import { useState, useMemo } from "react";
import { useChat, useShell, useContextPane } from "../context/AppContext";
import { useAgentStreamContext } from "../context/AgentStreamContext";
import { api } from "../lib/ipc";
import { workspaceSuggestions } from "../lib/emptyState";
import FileTreePanel from "../components/editor/FileTreePanel";
import EditorPane from "../components/editor/EditorPane";
import TerminalDrawer from "../components/editor/TerminalDrawer";
import AgentChatSurface from "../components/AgentChatSurface";
import ChatComposer from "../components/ChatComposer";
import "./WorkspacePage.css";

function WorkspacePage() {
  const { messages } = useChat();
  const { projectPath, setProjectPath } = useShell();
  const { addContextPath } = useContextPane();
  const { liveTools } = useAgentStreamContext();
  const [selected, setSelected] = useState<string | null>(null);
  const [focusLine, setFocusLine] = useState<number | null>(null);
  const [treeW, setTreeW] = useState(240);
  const [chatW, setChatW] = useState(400);
  const [dragging, setDragging] = useState<null | "tree" | "chat">(null);
  const [termOpen, setTermOpen] = useState(false);
  const suggestions = useMemo(
    () => workspaceSuggestions(selected ? { path: selected, content: "" } : null),
    [selected],
  );

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
        <FileTreePanel
          projectPath={projectPath}
          selectedPath={selected}
          onSelect={(p) => {
            setSelected(p);
            setFocusLine(null);
          }}
          onOpenHit={(p, line) => {
            setSelected(p);
            setFocusLine(line);
          }}
        />
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
          focusLine={focusLine}
        />
        <TerminalDrawer projectPath={projectPath} open={termOpen} onToggle={() => setTermOpen((v) => !v)} />
      </div>
      <div
        className={`workspace-divider${dragging === "chat" ? " dragging" : ""}`}
        onPointerDown={startDrag("chat")}
        title="Drag to resize"
      />
      <div className="workspace-pane workspace-chat">
        <div className="workspace-chat-header">
          <span>Chat</span>
          <button
            type="button"
            className="btn-index"
            onClick={() => void handleBrowse()}
            title="Change project folder"
          >
            {projectPath ? projectPath.split(/[/\\]/).pop() : "Set workspace"}
          </button>
        </div>
        <AgentChatSurface
          messages={messages}
          liveTools={liveTools}
          scrollClassName="workspace-chat-scroll"
          emptyState={
            <div className="workspace-chat-empty">
              <p>Ask the agent to explore or edit code — changes appear in the editor pane.</p>
              <div className="workspace-chat-suggestions">
                {suggestions.slice(0, 4).map((s) => (
                  <button
                    key={s.prompt}
                    type="button"
                    className="chat-suggestion"
                    onClick={() =>
                      window.dispatchEvent(
                        new CustomEvent("vencoder-prefill-composer", { detail: s.prompt }),
                      )
                    }
                  >
                    {s.label}
                  </button>
                ))}
              </div>
            </div>
          }
        />
        <div className="workspace-composer">
          <ChatComposer variant="bottom" />
        </div>
      </div>
    </div>
  );
}

export default WorkspacePage;