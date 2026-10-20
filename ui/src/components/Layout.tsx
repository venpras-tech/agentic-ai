import { useEffect } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import TitleBar from "./TitleBar";
import DownloadBar from "./DownloadBar";
import Sidebar from "./Sidebar";
import BottomBar from "./BottomBar";
import HistoryPanel from "./HistoryPanel";
import StatusBanners from "./StatusBanners";
import IndexStatusChip from "./IndexStatusChip";
import GlobalShortcuts from "./GlobalShortcuts";
import CommandPalette from "./CommandPalette";
import NeedsModelDialog from "./NeedsModelDialog";
import { useModel } from "../context/AppContext";
import { AgentStreamProvider } from "../context/AgentStreamContext";
import type { OpenEditorTarget } from "../lib/ipc";
import "../styles/layout.css";

function Layout({ children }: { children: React.ReactNode }) {
  const location = useLocation();
  const navigate = useNavigate();
  const { modelPromptVisible, dismissModelPrompt, modelRestoreError, lastModel } = useModel();
  const showChatBar = location.pathname === "/chat";

  useEffect(() => {
    const onOpenInEditor = (e: Event) => {
      const detail = (e as CustomEvent<OpenEditorTarget | null>).detail;
      if (!detail) return;
      const path = typeof detail === "string" ? detail : detail.path;
      if (!path) return;
      const line = typeof detail === "string" ? undefined : detail.line;
      navigate(`/editor?path=${encodeURIComponent(path)}${line ? `&line=${line}` : ""}`);
    };
    window.addEventListener("vencoder-open-editor", onOpenInEditor);
    return () => window.removeEventListener("vencoder-open-editor", onOpenInEditor);
  }, [navigate]);

  return (
    <div className="app">
      <GlobalShortcuts />
      <CommandPalette />
      <TitleBar />
      <DownloadBar />
      <a href="#page-main" className="skip-link">
        Skip to main content
      </a>
      <header className="app-top-status" aria-label="Connection status">
        <StatusBanners />
        <IndexStatusChip />
      </header>
      <div className="app-body">
        <Sidebar />
        <HistoryPanel />
        <main id="page-main" className="main" tabIndex={-1}>
          <AgentStreamProvider>
            <div className="page-content">{children}</div>
            {showChatBar ? <BottomBar /> : null}
          </AgentStreamProvider>
        </main>
      </div>
      <NeedsModelDialog
        open={modelPromptVisible}
        error={modelRestoreError}
        modelLabel={lastModel?.name}
        onProceed={() => {
          dismissModelPrompt();
          navigate("/models");
        }}
        onDismiss={dismissModelPrompt}
      />
    </div>
  );
}

export default Layout;