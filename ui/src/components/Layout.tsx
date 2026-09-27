import { useEffect } from "react";
import { useLocation, useNavigate } from "react-router-dom";
import TitleBar from "./TitleBar";
import DownloadBar from "./DownloadBar";
import Sidebar from "./Sidebar";
import BottomBar from "./BottomBar";
import HistoryPanel from "./HistoryPanel";
import StatusBanners from "./StatusBanners";
import GlobalShortcuts from "./GlobalShortcuts";
import NeedsModelDialog from "./NeedsModelDialog";
import { useApp } from "../context/AppContext";
import "../styles/layout.css";

function Layout({ children }: { children: React.ReactNode }) {
  const location = useLocation();
  const navigate = useNavigate();
  const { modelPromptVisible, dismissModelPrompt, modelRestoreError, lastModel } = useApp();
  const showChatBar = location.pathname === "/chat";

  useEffect(() => {
    const onOpenInEditor = (e: Event) => {
      const path = (e as CustomEvent<string>).detail;
      if (!path) return;
      navigate(`/editor?path=${encodeURIComponent(path)}`);
    };
    window.addEventListener("vencoder-open-editor", onOpenInEditor);
    return () => window.removeEventListener("vencoder-open-editor", onOpenInEditor);
  }, [navigate]);

  return (
    <div className="app">
      <GlobalShortcuts />
      <TitleBar />
      <DownloadBar />
      <a href="#page-main" className="skip-link">
        Skip to main content
      </a>
      <header className="app-top-status" aria-label="Connection status">
        <StatusBanners />
      </header>
      <div className="app-body">
        <Sidebar />
        <HistoryPanel />
        <main id="page-main" className="main" tabIndex={-1}>
          <div className="page-content">{children}</div>
          {showChatBar ? <BottomBar /> : null}
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