import { useEffect } from "react";
import { useNavigate } from "react-router-dom";
import { useChat, useShell } from "../context/AppContext";
import { api } from "../lib/ipc";

function GlobalShortcuts() {
  const navigate = useNavigate();
  const { isProcessing, newChatSession, currentMode, setMode } = useChat();
  const { toggleHistoryPanel, historyPanelOpen, setHistoryPanelOpen } = useShell();

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // Shift+Tab cycles permission modes globally, except while typing in a
      // field (the composer handles its own Shift+Tab there).
      if (e.key === "Tab" && e.shiftKey) {
        const el = document.activeElement as HTMLElement | null;
        const tag = el?.tagName;
        const editable =
          tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || el?.isContentEditable;
        if (!editable) {
          e.preventDefault();
          const order = ["agent", "plan", "ask"];
          const i = order.indexOf(currentMode);
          setMode(order[(i + 1) % order.length] ?? "agent");
        }
        return;
      }

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
    toggleHistoryPanel,
    setHistoryPanelOpen,
    historyPanelOpen,
    newChatSession,
    navigate,
    currentMode,
    setMode,
  ]);

  return null;
}

export default GlobalShortcuts;