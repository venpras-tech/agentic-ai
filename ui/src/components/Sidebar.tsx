import { useEffect, useCallback, useRef, useState } from "react";
import { useNavigate, useLocation } from "react-router-dom";
import { useShell, useModel, useChat } from "../context/AppContext";
import { api, isTauriRuntime } from "../lib/ipc";
import WorkspaceSwitcher from "./WorkspaceSwitcher";
import "./Sidebar.css";

function HomeIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
      <polyline points="9 22 9 12 15 12 15 22" />
    </svg>
  );
}

function ChatIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z" />
    </svg>
  );
}

function ProjectIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
    </svg>
  );
}

function EditorIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
      <path d="M9 14h6M9 17h4" />
    </svg>
  );
}

function WorkspaceIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <rect x="3" y="3" width="7" height="9" rx="1" />
      <rect x="14" y="3" width="7" height="5" rx="1" />
      <rect x="14" y="12" width="7" height="9" rx="1" />
      <rect x="3" y="16" width="7" height="5" rx="1" />
    </svg>
  );
}

function ModelsIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M9.5 2A2.5 2.5 0 0 1 12 4.5v15a2.5 2.5 0 0 1-4.96.44 2.5 2.5 0 0 1-2.96-3.08 3 3 0 0 1-.34-5.58 2.5 2.5 0 0 1 1.32-4.24 2.5 2.5 0 0 1 1.98-3A2.5 2.5 0 0 1 9.5 2Z" />
      <path d="M14.5 2A2.5 2.5 0 0 0 12 4.5v15a2.5 2.5 0 0 0 4.96.44 2.5 2.5 0 0 0 2.96-3.08 3 3 0 0 0 .34-5.58 2.5 2.5 0 0 0-1.32-4.24 2.5 2.5 0 0 0-1.98-3A2.5 2.5 0 0 0 14.5 2Z" />
    </svg>
  );
}

function SettingsIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

function LogsIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
      <line x1="16" y1="13" x2="8" y2="13" />
      <line x1="16" y1="17" x2="8" y2="17" />
      <polyline points="10 9 9 9 8 9" />
    </svg>
  );
}

function HelpIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
      <circle cx="12" cy="12" r="10" />
      <path d="M9.09 9a3 3 0 0 1 5.83 1c0 2-3 3-3 3" />
      <path d="M12 17h.01" />
    </svg>
  );
}

function ShieldIcon() {
  return (
    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" />
    </svg>
  );
}

const navItems = [
  { path: "/", page: "home", label: "Home", icon: HomeIcon },
  { path: "/chat", page: "chat", label: "Chat", icon: ChatIcon },
  { path: "/project", page: "project", label: "Project", icon: ProjectIcon },
  { path: "/editor", page: "editor", label: "Editor", icon: EditorIcon },
  { path: "/workspace", page: "workspace", label: "Split View", icon: WorkspaceIcon },
  { path: "/models", page: "models", label: "Models", icon: ModelsIcon },
  { path: "/settings", page: "settings", label: "Settings", icon: SettingsIcon },
  { path: "/security", page: "security", label: "Security", icon: ShieldIcon },
  { path: "/logs", page: "logs", label: "Logs", icon: LogsIcon },
  { path: "/help", page: "help", label: "Help", icon: HelpIcon },
];

function Sidebar() {
  const {
    sidebarCollapsed,
    toggleSidebar,
    projectPath,
    backendConnected,
    setBackendConnected,
    toggleHistoryPanel,
  } = useShell();
  const { modelName, modelProvider } = useModel();
  const { newChatSession } = useChat();
  const navigate = useNavigate();
  const location = useLocation();

  const [connState, setConnState] = useState<"connecting" | "connected" | "disconnected" | "reconnecting">(
    isTauriRuntime() ? "connecting" : "connected",
  );
  const retryTimerRef = useRef<number | null>(null);
  const attemptRef = useRef(0);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      if (retryTimerRef.current) window.clearTimeout(retryTimerRef.current);
    };
  }, []);

  const checkBackend = useCallback(async () => {
    if (!isTauriRuntime()) return;
    attemptRef.current += 1;
    try {
      await api.modelStatus();
      if (!mountedRef.current) return;
      attemptRef.current = 0;
      setBackendConnected(true);
      setConnState("connected");
    } catch {
      if (!mountedRef.current) return;
      setBackendConnected(false);
      setConnState("disconnected");
      const delay = Math.min(30000, 1000 * 2 ** (attemptRef.current - 1));
      if (retryTimerRef.current) window.clearTimeout(retryTimerRef.current);
      retryTimerRef.current = window.setTimeout(() => void checkBackend(), delay);
    }
  }, [setBackendConnected]);

  const retryNow = useCallback(() => {
    if (!isTauriRuntime()) return;
    if (retryTimerRef.current) window.clearTimeout(retryTimerRef.current);
    if (backendConnected) return;
    attemptRef.current = 0;
    setConnState("reconnecting");
    void checkBackend();
  }, [backendConnected, checkBackend]);

  useEffect(() => {
    if (!isTauriRuntime()) {
      setConnState("connected");
      setBackendConnected(true);
      return;
    }
    setConnState("connecting");
    void checkBackend();
  }, [checkBackend, setBackendConnected]);

  const handleNavClick = (path: string) => navigate(path);
  const currentPath = location.pathname;

  const handleNewChat = () => {
    newChatSession();
    navigate("/chat");
  };

  const homeItem = navItems[0];
  const chatItem = navItems[1];
  const restNav = navItems.slice(2);
  const HomeIconComp = homeItem.icon;
  const ChatIconComp = chatItem.icon;

  return (
    <aside className={`sidebar ${sidebarCollapsed ? "collapsed" : ""}`}>
      <div className="sidebar-header">
        <span className="sidebar-logo">
          <span className="sidebar-logo-icon">AI</span>
          AI Editor
        </span>
        <div className="sidebar-header-actions">
          <button
            type="button"
            className="sidebar-new-btn"
            onClick={handleNewChat}
            title="New chat (Ctrl+N)"
            aria-label="New chat (Ctrl+N)"
          >
            <span className="sidebar-new-plus" aria-hidden="true">+</span>
            <span className="sidebar-new-text">New</span>
          </button>
          <button
            type="button"
            className="sidebar-toggle"
            onClick={toggleSidebar}
            title="Toggle sidebar"
            aria-label="Toggle sidebar"
          >
            <svg className="sidebar-toggle-icon" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M3 6h18" />
              <path d="M3 12h18" />
              <path d="M3 18h18" />
            </svg>
            <svg className="sidebar-toggle-icon collapsed" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M9 18l6-6-6-6" />
            </svg>
          </button>
        </div>
      </div>
      <nav className="side-nav">
        <button
          type="button"
          className={`side-nav-item ${currentPath === homeItem.path ? "active" : ""}`}
          onClick={() => handleNavClick(homeItem.path)}
          data-page={homeItem.page}
          aria-current={currentPath === homeItem.path ? "page" : undefined}
        >
          <span className="side-nav-icon">
            <HomeIconComp />
          </span>
          <span className="side-nav-text">{homeItem.label}</span>
        </button>
        <div className="side-nav-chat-wrap">
          <button
            type="button"
            className={`side-nav-item ${currentPath === chatItem.path ? "active" : ""}`}
            onClick={() => handleNavClick(chatItem.path)}
            data-page={chatItem.page}
            aria-current={currentPath === chatItem.path ? "page" : undefined}
          >
            <span className="side-nav-icon">
              <ChatIconComp />
            </span>
            <span className="side-nav-text">{chatItem.label}</span>
          </button>
          <div className="side-nav-sub">
            <button type="button" className="nav-sub-item" onClick={handleNewChat}>
              New chat
            </button>
            <button type="button" className="nav-sub-item" onClick={toggleHistoryPanel}>
              Chat history
            </button>
          </div>
        </div>
        {restNav.map((item) => {
          const Icon = item.icon;
          const isActive = currentPath === item.path;
          return (
            <div key={item.page} className="side-nav-chat-wrap">
              <button
                type="button"
                className={`side-nav-item ${isActive ? "active" : ""}`}
                onClick={() => handleNavClick(item.path)}
                data-page={item.page}
                aria-current={isActive ? "page" : undefined}
              >
                <span className="side-nav-icon">
                  <Icon />
                </span>
                <span className="side-nav-text">{item.label}</span>
              </button>
              {item.page === "models" ? (
                <div className="side-nav-sub">
                  <button
                    type="button"
                    className={`nav-sub-item ${currentPath === "/models/hub" ? "active" : ""}`}
                    onClick={() => handleNavClick("/models/hub")}
                  >
                    Download models
                  </button>
                </div>
              ) : null}
            </div>
          );
        })}
        <div className="side-nav-sep" />
        <div className="side-nav-details">
          <button
            type="button"
            className={`side-nav-status ${connState}`}
            onClick={retryNow}
            title={
              connState === "connected"
                ? "Backend connected — code, chat, and run tools ready"
                : connState === "connecting"
                  ? "Starting backend…"
                  : connState === "reconnecting"
                    ? "Retrying backend connection…"
                    : "Backend disconnected — auto-retrying. Click to retry now."
            }
          >
            <span className={`side-nav-status-dot ${connState}`} aria-hidden="true" />
            <span className="side-nav-status-model">{modelName || (connState === "connected" ? "Backend" : "Backend offline")}</span>
            <span className={`side-nav-status-sub ${connState}`}>
              {connState === "connected"
                ? modelProvider || "Local"
                : connState === "connecting"
                  ? "connecting…"
                  : connState === "reconnecting"
                    ? "reconnecting…"
                    : "disconnected"}
            </span>
          </button>
          <div className="side-nav-project-footer" title={projectPath || ""}>
            Project: {projectPath ? projectPath.split(/[/\\]/).pop() : "—"}
          </div>
          <WorkspaceSwitcher />
        </div>
      </nav>
    </aside>
  );
}

export default Sidebar;