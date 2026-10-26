import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChat, useShell, useContextPane } from "../context/AppContext";
import { useAgentStreamContext } from "../context/AgentStreamContext";
import { api, isTauriRuntime, type OpenEditorTarget } from "../lib/ipc";
import { workspaceSuggestions } from "../lib/emptyState";
import FileTreePanel from "../components/editor/FileTreePanel";
import EditorPane from "../components/editor/EditorPane";
import TerminalDrawer from "../components/editor/TerminalDrawer";
import AgentChatSurface from "../components/AgentChatSurface";
import ChatComposer from "../components/ChatComposer";
import "./WorkspacePage.css";

type LayoutTier = "desktop" | "tablet" | "phone";
type DragKind = null | "tree" | "chat" | "term";

function layoutTier(width: number): LayoutTier {
  if (width < 640) return "phone";
  if (width < 1100) return "tablet";
  return "desktop";
}

function baseName(path: string): string {
  return path.split(/[/\\]/).pop() || path;
}

function ExplorerIcon() {
  return (
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" aria-hidden="true">
      <path d="M3 7v12a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-6l-2-2H5a2 2 0 0 0-2 2z" />
    </svg>
  );
}

function FolderOpenIcon() {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      <path d="M3 7v12a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V9a2 2 0 0 0-2-2h-6l-2-2H5a2 2 0 0 0-2 2z" />
      <path d="M3 7h6l2 2h10" />
    </svg>
  );
}

function ChatSideIcon() {
  return (
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" aria-hidden="true">
      <path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z" />
    </svg>
  );
}

function TerminalIcon() {
  return (
    <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.75" aria-hidden="true">
      <polyline points="4 17 10 11 4 5" />
      <line x1="12" y1="19" x2="20" y2="19" />
    </svg>
  );
}

function WorkspacePage() {
  const { messages } = useChat();
  const { projectPath, setProjectPath } = useShell();
  const { addContextPath } = useContextPane();
  const { liveTools } = useAgentStreamContext();
  const [openTabs, setOpenTabs] = useState<string[]>([]);
  const [activePath, setActivePath] = useState<string | null>(null);
  const [focusLine, setFocusLine] = useState<number | null>(null);
  const [treeW, setTreeW] = useState(240);
  const [chatW, setChatW] = useState(340);
  const [termH, setTermH] = useState(200);
  const [dragging, setDragging] = useState<DragKind>(null);
  const [explorerOpen, setExplorerOpen] = useState(true);
  const [chatOpen, setChatOpen] = useState(true);
  const [termOpen, setTermOpen] = useState(false);
  const [tier, setTier] = useState<LayoutTier>(() => layoutTier(window.innerWidth));
  const [overlay, setOverlay] = useState<null | "explorer" | "chat">(null);
  const [dirtyPaths, setDirtyPaths] = useState<Set<string>>(() => new Set());
  const [treeRefresh, setTreeRefresh] = useState(0);
  const layoutTouched = useRef(false);
  const prevProjectRef = useRef(projectPath);

  const suggestions = useMemo(
    () => workspaceSuggestions(activePath ? { path: activePath, content: "" } : null),
    [activePath],
  );

  const openFile = useCallback((path: string, line?: number | null) => {
    setOpenTabs((prev) => (prev.includes(path) ? prev : [...prev, path]));
    setActivePath(path);
    setFocusLine(line ?? null);
  }, []);

  const closeTab = useCallback(
    (path: string) => {
      setOpenTabs((prev) => {
        const next = prev.filter((p) => p !== path);
        if (activePath === path) {
          const idx = prev.indexOf(path);
          const fallback = next[Math.min(idx, next.length - 1)] ?? next[next.length - 1] ?? null;
          setActivePath(fallback);
          setFocusLine(null);
        }
        return next;
      });
      setDirtyPaths((prev) => {
        const next = new Set(prev);
        next.delete(path);
        return next;
      });
    },
    [activePath],
  );

  useEffect(() => {
    const onResize = () => {
      const next = layoutTier(window.innerWidth);
      setTier(next);
      const maxChat = Math.max(260, Math.floor(window.innerWidth * 0.36));
      setChatW((w) => Math.min(w, maxChat));
      if (!layoutTouched.current) {
        if (next === "phone") {
          setExplorerOpen(false);
          setChatOpen(false);
        } else if (next === "tablet") {
          setExplorerOpen(true);
          setChatOpen(false);
        } else {
          setExplorerOpen(true);
          setChatOpen(true);
        }
      }
    };
    onResize();
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);

  useEffect(() => {
    if (tier === "desktop") setOverlay(null);
  }, [tier]);

  useEffect(() => {
    if (prevProjectRef.current !== projectPath) {
      prevProjectRef.current = projectPath;
      if (!projectPath) {
        setOpenTabs([]);
        setActivePath(null);
        setFocusLine(null);
      }
    }
  }, [projectPath]);

  useEffect(() => {
    const listener = (e: Event) => {
      const detail = (e as CustomEvent<OpenEditorTarget | null>).detail;
      if (!detail) return;
      const path = typeof detail === "string" ? detail : detail.path;
      const line = typeof detail === "string" ? undefined : detail.line;
      if (path) {
        setTreeRefresh((n) => n + 1);
        openFile(path, line ?? null);
      }
    };
    window.addEventListener("vencoder-open-editor", listener);
    return () => window.removeEventListener("vencoder-open-editor", listener);
  }, [openFile]);

  useEffect(() => {
    document.title = "Workspace — Vencoder";
    return () => {
      document.title = "Vencoder";
    };
  }, []);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    void listen("workspace://file-changed", () => {
      setTreeRefresh((n) => n + 1);
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const handleBrowse = async () => {
    try {
      const path = await api.pickWorkspaceFolder();
      if (path) {
        setOpenTabs([]);
        setActivePath(null);
        setFocusLine(null);
        setDirtyPaths(new Set());
        setProjectPath(path);
        await api.agentSetWorkspace(path);
      }
    } catch {
      // dialog dismissed
    }
  };

  const startDrag = useCallback(
    (which: Exclude<DragKind, null>) => (e: React.PointerEvent<HTMLDivElement>) => {
      if (tier !== "desktop") return;
      e.preventDefault();
      const startX = e.clientX;
      const startY = e.clientY;
      const startTree = treeW;
      const startChat = chatW;
      const startTerm = termH;
      const onMove = (ev: PointerEvent) => {
        if (which === "tree") {
          setTreeW(Math.min(420, Math.max(180, startTree + ev.clientX - startX)));
        } else if (which === "chat") {
          const maxChat = Math.max(260, Math.floor(window.innerWidth * 0.42));
          setChatW(Math.min(maxChat, Math.max(260, startChat + startX - ev.clientX)));
        } else if (which === "term") {
          setTermH(Math.min(420, Math.max(100, startTerm + startY - ev.clientY)));
        }
      };
      const onUp = () => {
        setDragging(null);
        document.body.style.userSelect = "";
        document.body.style.cursor = "";
        window.removeEventListener("pointermove", onMove);
      };
      setDragging(which);
      document.body.style.userSelect = "none";
      document.body.style.cursor = which === "term" ? "row-resize" : "col-resize";
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp, { once: true });
    },
    [tier, treeW, chatW, termH],
  );

  const toggleExplorer = () => {
    layoutTouched.current = true;
    if (tier === "desktop") {
      setExplorerOpen((v) => !v);
      return;
    }
    setOverlay((o) => (o === "explorer" ? null : "explorer"));
  };

  const toggleChat = () => {
    layoutTouched.current = true;
    if (tier === "desktop") {
      setChatOpen((v) => !v);
      return;
    }
    setOverlay((o) => (o === "chat" ? null : "chat"));
  };

  const explorerVisible = tier === "desktop" ? explorerOpen : overlay === "explorer";
  const chatVisible = tier === "desktop" ? chatOpen : overlay === "chat";

  const gridColumns = useMemo(() => {
    if (tier !== "desktop") return "minmax(0, 1fr)";
    const parts: string[] = [];
    if (explorerOpen) parts.push(`${treeW}px`, "4px");
    parts.push("minmax(0, 1fr)");
    if (chatOpen) parts.push("4px", `${chatW}px`);
    return parts.join(" ");
  }, [tier, explorerOpen, chatOpen, treeW, chatW]);

  const explorerHeaderActions = (
    <div className="workspace-sidebar-actions">
      <button type="button" className="workspace-icon-btn" onClick={() => void handleBrowse()} title="Open Folder">
        <FolderOpenIcon />
      </button>
      {tier === "desktop" ? (
        <button type="button" className="workspace-icon-btn" onClick={() => setExplorerOpen(false)} title="Hide explorer">
          ×
        </button>
      ) : null}
    </div>
  );

  const treePanel = (closeOnPick: boolean) => (
    <FileTreePanel
      projectPath={projectPath}
      selectedPath={activePath}
      refreshSignal={treeRefresh}
      onOpenFolder={() => void handleBrowse()}
      onAddContext={(rel) => addContextPath(rel)}
      onSelect={(p) => {
        openFile(p, null);
        if (closeOnPick) setOverlay(null);
      }}
      onOpenHit={(p, line) => {
        openFile(p, line);
        if (closeOnPick) setOverlay(null);
      }}
    />
  );

  return (
    <div className={`workspace-ide workspace-tier-${tier}${overlay ? ` workspace-overlay-${overlay}` : ""}`}>
      <nav className="workspace-activity" aria-label="Primary side bar">
        <button
          type="button"
          className={`workspace-activity-btn${explorerVisible ? " active" : ""}`}
          onClick={toggleExplorer}
          title="Explorer (Ctrl+Shift+E)"
          aria-pressed={explorerVisible}
        >
          <ExplorerIcon />
        </button>
        <button
          type="button"
          className={`workspace-activity-btn${termOpen ? " active" : ""}`}
          onClick={() => {
            layoutTouched.current = true;
            setTermOpen((v) => !v);
          }}
          title="Terminal (Ctrl+`)"
          aria-pressed={termOpen}
        >
          <TerminalIcon />
        </button>
        <button
          type="button"
          className={`workspace-activity-btn${chatVisible ? " active" : ""}`}
          onClick={toggleChat}
          title="Chat"
          aria-pressed={chatVisible}
        >
          <ChatSideIcon />
        </button>
      </nav>

      <div className="workspace-shell">
        {tier !== "desktop" && overlay ? (
          <button
            type="button"
            className="workspace-backdrop"
            aria-label="Close panel"
            onClick={() => setOverlay(null)}
          />
        ) : null}

        <div className="workspace-grid" style={{ gridTemplateColumns: gridColumns }}>
          {tier === "desktop" && explorerOpen ? (
            <aside className="workspace-sidebar workspace-explorer" aria-label="Explorer">
              <header className="workspace-sidebar-title">
                <span>Explorer</span>
                {explorerHeaderActions}
              </header>
              {treePanel(false)}
            </aside>
          ) : null}

          {tier === "desktop" && explorerOpen ? (
            <div
              className={`workspace-sash workspace-sash-v${dragging === "tree" ? " dragging" : ""}`}
              onPointerDown={startDrag("tree")}
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize explorer"
            />
          ) : null}

          <section className="workspace-center" aria-label="Editor">
            <div className="workspace-tabbar">
              <div className="workspace-tabs" role="tablist">
                {openTabs.length === 0 ? (
                  <div className="workspace-tab workspace-tab-muted active" role="tab" aria-selected={true}>
                    <span className="workspace-tab-label">Welcome</span>
                  </div>
                ) : (
                  openTabs.map((path) => {
                    const active = path === activePath;
                    return (
                      <button
                        key={path}
                        type="button"
                        role="tab"
                        aria-selected={active}
                        className={`workspace-tab${active ? " active" : ""}`}
                        onClick={() => {
                          setActivePath(path);
                          setFocusLine(null);
                        }}
                        title={path}
                      >
                        {dirtyPaths.has(path) ? (
                          <span className="workspace-tab-dirty" title="Unsaved changes">●</span>
                        ) : null}
                        <span className="workspace-tab-label">{baseName(path)}</span>
                        <span
                          className="workspace-tab-close"
                          role="button"
                          tabIndex={-1}
                          aria-label={`Close ${baseName(path)}`}
                          onClick={(e) => {
                            e.stopPropagation();
                            closeTab(path);
                          }}
                        >
                          ×
                        </span>
                      </button>
                    );
                  })
                )}
              </div>
              <div className="workspace-tabbar-actions">
                <button type="button" className="workspace-toolbar-btn primary" onClick={() => void handleBrowse()} title="Open Folder">
                  <FolderOpenIcon />
                  <span>Open Folder</span>
                </button>
              </div>
            </div>

            <div
              className="workspace-editor-stack"
              style={
                termOpen
                  ? { gridTemplateRows: `minmax(0, 1fr) 4px ${termH}px` }
                  : { gridTemplateRows: "minmax(0, 1fr)" }
              }
            >
              <div className="workspace-editor-pane">
                {!projectPath && openTabs.length === 0 ? (
                  <div className="workspace-welcome">
                    <h2>Open a folder</h2>
                    <p>Start by opening a project folder to browse files, edit in tabs, and run the agent.</p>
                    <button type="button" className="workspace-welcome-btn" onClick={() => void handleBrowse()}>
                      Open Folder…
                    </button>
                  </div>
                ) : (
                  <EditorPane
                    key={activePath ?? "__welcome__"}
                    selectedPath={activePath}
                    projectPath={projectPath}
                    onAddContext={(rel) => addContextPath(rel)}
                    focusLine={focusLine}
                    showTitleInHeader={false}
                    onDirtyChange={(d) => {
                      if (!activePath) return;
                      setDirtyPaths((prev) => {
                        const next = new Set(prev);
                        if (d) next.add(activePath);
                        else next.delete(activePath);
                        return next;
                      });
                    }}
                  />
                )}
              </div>

              {termOpen ? (
                <div
                  className={`workspace-sash workspace-sash-h${dragging === "term" ? " dragging" : ""}`}
                  onPointerDown={startDrag("term")}
                  role="separator"
                  aria-orientation="horizontal"
                  aria-label="Resize terminal"
                />
              ) : null}

              {termOpen ? (
                <div className="workspace-panel">
                  <div className="workspace-panel-tabs">
                    <button type="button" className="workspace-panel-tab active">Terminal</button>
                    <button type="button" className="workspace-panel-tab" onClick={() => setTermOpen(false)} title="Close panel">
                      ×
                    </button>
                  </div>
                  <TerminalDrawer
                    projectPath={projectPath}
                    open={termOpen}
                    onToggle={() => setTermOpen((v) => !v)}
                    variant="panel"
                    hideHeader
                  />
                </div>
              ) : null}
            </div>

            <footer className="workspace-statusbar">
              <button type="button" className="workspace-status-btn" onClick={() => void handleBrowse()} title="Change folder">
                {projectPath ? baseName(projectPath) : "Open Folder"}
              </button>
              {activePath ? <span className="workspace-status-path">{activePath}</span> : null}
              {openTabs.length > 0 ? (
                <span className="workspace-status-meta">{openTabs.length} open</span>
              ) : null}
            </footer>
          </section>

          {tier === "desktop" && chatOpen ? (
            <div
              className={`workspace-sash workspace-sash-v${dragging === "chat" ? " dragging" : ""}`}
              onPointerDown={startDrag("chat")}
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize chat"
            />
          ) : null}

          {tier === "desktop" && chatOpen ? (
            <aside className="workspace-sidebar workspace-chat" aria-label="Chat">
              <header className="workspace-sidebar-title">
                <span>Chat</span>
                <button type="button" className="workspace-icon-btn" onClick={() => setChatOpen(false)} title="Hide chat">
                  ×
                </button>
              </header>
              <div className="workspace-chat-body">
                <AgentChatSurface
                  messages={messages}
                  liveTools={liveTools}
                  scrollClassName="workspace-chat-scroll"
                  emptyState={
                    <div className="workspace-chat-empty">
                      <p>Ask the agent to explore or edit code — changes appear in the editor.</p>
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
              </div>
              <div className="workspace-composer">
                <ChatComposer />
              </div>
            </aside>
          ) : null}
        </div>

        {tier !== "desktop" && overlay === "explorer" ? (
          <aside className="workspace-flyout workspace-flyout-explorer" aria-label="Explorer">
            <header className="workspace-sidebar-title">
              <span>Explorer</span>
              <div className="workspace-sidebar-actions">
                <button type="button" className="workspace-icon-btn" onClick={() => void handleBrowse()} title="Open Folder">
                  <FolderOpenIcon />
                </button>
                <button type="button" className="workspace-icon-btn" onClick={() => setOverlay(null)}>×</button>
              </div>
            </header>
            {treePanel(tier === "phone")}
          </aside>
        ) : null}

        {tier !== "desktop" && overlay === "chat" ? (
          <aside className="workspace-flyout workspace-flyout-chat" aria-label="Chat">
            <header className="workspace-sidebar-title">
              <span>Chat</span>
              <button type="button" className="workspace-icon-btn" onClick={() => setOverlay(null)}>×</button>
            </header>
            <div className="workspace-chat-body">
              <AgentChatSurface
                messages={messages}
                liveTools={liveTools}
                scrollClassName="workspace-chat-scroll"
                emptyState={<div className="workspace-chat-empty"><p>Agent chat for this workspace.</p></div>}
              />
            </div>
            <div className="workspace-composer">
              <ChatComposer />
            </div>
          </aside>
        ) : null}
      </div>
    </div>
  );
}

export default WorkspacePage;
