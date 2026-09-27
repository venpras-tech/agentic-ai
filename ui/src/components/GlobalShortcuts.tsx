import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { useApp } from "../context/AppContext";
import { api } from "../lib/ipc";

function GlobalShortcuts() {
  const navigate = useNavigate();
  const {
    isProcessing,
    toggleHistoryPanel,
    historyPanelOpen,
    setHistoryPanelOpen,
    newChatSession,
    chatAbortRef,
  } = useApp();

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (isProcessing) {
          e.preventDefault();
          void api.abortAgentExecution().catch(() => {});
          return;
        }
        if (historyPanelOpen) {
          e.preventDefault();
          setHistoryPanelOpen(false);
        }
        return;
      }

      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;

      if (e.key === "n" || e.key === "N") {
        e.preventDefault();
        newChatSession();
        navigate("/chat");
        return;
      }

      if (e.key === "k" || e.key === "K") {
        e.preventDefault();
        window.dispatchEvent(new CustomEvent("vencoder-focus-composer"));
        return;
      }

      if (e.shiftKey && (e.key === "H" || e.key === "h")) {
        e.preventDefault();
        toggleHistoryPanel();
        return;
      }

      if (e.key >= "1" && e.key <= "5" && !e.altKey && !e.shiftKey) {
        const routes = ["/", "/chat", "/project", "/models", "/settings"];
        const idx = parseInt(e.key, 10) - 1;
        if (routes[idx]) {
          e.preventDefault();
          navigate(routes[idx]);
        }
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [
    isProcessing,
    chatAbortRef,
    toggleHistoryPanel,
    setHistoryPanelOpen,
    historyPanelOpen,
    newChatSession,
    navigate,
  ]);

  return null;
}

export default GlobalShortcuts;