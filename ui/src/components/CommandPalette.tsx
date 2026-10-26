import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useChat, useShell } from "../context/AppContext";
import { exportConversation } from "../lib/exportChat";
import "./CommandPalette.css";

interface Command {
  id: string;
  label: string;
  hint?: string;
  keywords?: string;
  run: () => void;
}

/**
 * Keyboard-driven command palette (`Ctrl/Cmd+Shift+P`). Mirrors the legacy
 * `src/` UI's cmdk palette: type to filter, arrow keys to move, Enter to run.
 * Self-contained (no external dependency) — it renders its own overlay and
 * carries its own global shortcut.
 */
function CommandPalette() {
  const navigate = useNavigate();
  const { newChatSession, clearMessages, messages } = useChat();
  const { toggleHistoryPanel, toggleSidebar, theme, setTheme } = useShell();
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);

  const commands = useMemo<Command[]>(() => {
    const close = () => setOpen(false);
    const go = (label: string, path: string, keywords?: string): Command => ({
      id: `nav:${path}`,
      label,
      hint: path,
      keywords,
      run: () => {
        navigate(path);
        close();
      },
    });
    const exportCmd = (format: "pdf" | "docx" | "csv"): Command => ({
      id: `export:${format}`,
      label: `Export chat as ${format.toUpperCase()}`,
      hint: "Save the conversation to a file",
      keywords: "download save conversation export pdf docx csv",
      run: () => {
        const firstUser = messages.find((m) => m.role === "user");
        const title = firstUser ? firstUser.content.slice(0, 60).replace(/\n/g, " ") : "Chat Export";
        void exportConversation({ messages, title, format }).catch((e) =>
          console.error("Export failed:", e),
        );
        close();
      },
    });
    return [
      {
        id: "chat:new",
        label: "New chat",
        hint: "Create a new conversation",
        keywords: "conversation session create",
        run: () => {
          newChatSession();
          navigate("/");
          close();
        },
      },
      {
        id: "chat:clear",
        label: "Clear conversation",
        hint: "Reset the current transcript",
        keywords: "reset messages delete",
        run: () => {
          clearMessages();
          close();
        },
      },
      {
        id: "chat:focus",
        label: "Focus composer",
        hint: "Jump to the message input",
        keywords: "input type message",
        run: () => {
          window.dispatchEvent(new CustomEvent("vencoder-focus-composer"));
          close();
        },
      },
      {
        id: "ui:history",
        label: "Toggle history panel",
        keywords: "chats sidebar sessions",
        run: () => {
          toggleHistoryPanel();
          close();
        },
      },
      {
        id: "ui:sidebar",
        label: "Toggle sidebar",
        keywords: "navigation collapse",
        run: () => {
          toggleSidebar();
          close();
        },
      },
      {
        id: "ui:theme",
        label: `Switch to ${theme === "dark" ? "light" : "dark"} theme`,
        keywords: "appearance color dark light",
        run: () => {
          setTheme(theme === "dark" ? "light" : "dark");
          close();
        },
      },
      ...(messages.length > 0
        ? [exportCmd("pdf"), exportCmd("docx"), exportCmd("csv")]
        : []),
      go("Go to Chat", "/", "agent conversation home"),
      go("Go to Workspace", "/workspace", "files explorer editor terminal"),
      go("Go to Editor", "/editor", "code file"),
      go("Go to Agent backends", "/agents", "openai claude ollama provider"),
      go("Go to Models (alias)", "/models", "llm provider load"),
      go("Go to Model Hub", "/models/hub", "download huggingface gguf"),
      go("Go to Images", "/images", "generate picture stable diffusion dalle"),
      go("Go to Video", "/video", "generate clip wan ltx"),
      go("Go to llama.cpp", "/llama-cpp", "gpu layers context bindings"),
      go("Go to Settings", "/settings", "preferences config"),
      go("Go to Security", "/security", "permissions policy"),
      go("Go to Logs", "/logs", "debug output"),
      go("Go to Help", "/help", "docs about"),
    ];
  }, [navigate, newChatSession, clearMessages, toggleHistoryPanel, toggleSidebar, theme, setTheme, messages]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return commands;
    return commands.filter((c) =>
      `${c.label} ${c.keywords ?? ""} ${c.hint ?? ""}`.toLowerCase().includes(q),
    );
  }, [commands, query]);

  useEffect(() => {
    setActive(0);
  }, [query, open]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.ctrlKey || e.metaKey;
      if (mod && e.shiftKey && (e.key === "p" || e.key === "P")) {
        e.preventDefault();
        setOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (open) {
      setQuery("");
      inputRef.current?.focus();
    }
  }, [open]);

  const runActive = useCallback(() => {
    filtered[active]?.run();
  }, [filtered, active]);

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Escape") {
      // Keep Escape from also reaching the global handler (which would abort a
      // running agent or close the history panel).
      e.preventDefault();
      e.stopPropagation();
      setOpen(false);
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setActive((a) => Math.min(a + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActive((a) => Math.max(a - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      runActive();
    }
  };

  if (!open) return null;

  return (
    <div className="cmdk-overlay" role="presentation" onClick={() => setOpen(false)}>
      <div
        className="cmdk"
        role="dialog"
        aria-modal="true"
        aria-label="Command palette"
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="cmdk-input"
          placeholder="Type a command or search…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          aria-label="Command search"
          aria-controls="cmdk-list"
          aria-expanded
          role="combobox"
          spellCheck={false}
        />
        <ul id="cmdk-list" className="cmdk-list" role="listbox" aria-label="Commands">
          {filtered.length === 0 ? (
            <li className="cmdk-empty">No matching commands</li>
          ) : (
            filtered.map((c, i) => (
              <li key={c.id} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={i === active}
                  className={`cmdk-item ${i === active ? "active" : ""}`}
                  onMouseEnter={() => setActive(i)}
                  onClick={c.run}
                >
                  <span className="cmdk-item-label">{c.label}</span>
                  {c.hint && <span className="cmdk-item-hint">{c.hint}</span>}
                </button>
              </li>
            ))
          )}
        </ul>
      </div>
    </div>
  );
}

export default CommandPalette;
