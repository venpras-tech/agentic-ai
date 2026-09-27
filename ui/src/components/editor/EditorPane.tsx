import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useApp } from "../../context/AppContext";
import { api } from "../../lib/ipc";
import { highlight, languageFor } from "./highlight";
import "./editor.css";

interface EditorPaneProps {
  selectedPath: string | null;
  projectPath?: string | null;
  onAddContext?: (relativePath: string) => void;
  onSave?: (path: string, content: string) => void;
}

function EditorPane({ selectedPath, projectPath, onAddContext, onSave }: EditorPaneProps) {
  const [content, setContent] = useState("");
  const [original, setOriginal] = useState("");
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [past, setPast] = useState<string[]>([]);
  const [future, setFuture] = useState<string[]>([]);
  const [selection, setSelection] = useState("");
  const navigate = useNavigate();
  const { setComposerDraft, setActiveFile } = useApp();

  const paneRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const layerRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef(content);
  contentRef.current = content;

  const dirty = selectedPath != null && content !== original;

  const openFile = useCallback(async (path: string) => {
    setMessage(null);
    try {
      const result = await api.readTextFile(path);
      setContent(result.content);
      setOriginal(result.content);
      setPast([]);
      setFuture([]);
    } catch (e) {
      setContent("");
      setOriginal("");
      setPast([]);
      setFuture([]);
      setMessage({ kind: "err", text: e instanceof Error ? e.message : String(e) });
    }
  }, []);

  useEffect(() => {
    if (selectedPath) void openFile(selectedPath);
  }, [selectedPath, openFile]);

  // Mirror the open file into app state (debounced) so the agent turn can pin
  // it as context. Pushing to the backend happens in prepareAgentTurn, which
  // composes the active file together with the user's context chips — pushing
  // here alone would be clobbered by that composition.
  useEffect(() => {
    if (!selectedPath || !content) {
      if (!selectedPath) setActiveFile(null);
      return;
    }
    const t = window.setTimeout(() => {
      setActiveFile({ path: selectedPath, content });
    }, 600);
    return () => window.clearTimeout(t);
  }, [selectedPath, content, setActiveFile]);

  const commitChange = (value: string) => {
    if (value === contentRef.current) return;
    setPast((p) => [...p.slice(-99), contentRef.current]);
    setFuture([]);
    setContent(value);
  };

  const refreshSelection = () => {
    const el = textareaRef.current;
    if (!el) {
      setSelection("");
      return;
    }
    const s = el.value.slice(el.selectionStart, el.selectionEnd);
    setSelection(el.selectionEnd > el.selectionStart ? s : "");
  };

  const askAI = () => {
    const sel = selection.trim();
    if (!sel) return;
    const prompt = `About this code: ${sel}`;
    setComposerDraft(prompt);
    window.dispatchEvent(new CustomEvent("vencoder-prefill-composer", { detail: prompt }));
    setSelection("");
    navigate("/chat");
  };

  const undo = useCallback(() => {
    if (past.length === 0) return;
    const prev = past[past.length - 1];
    setPast(past.slice(0, -1));
    setFuture([contentRef.current, ...future].slice(0, 100));
    setContent(prev);
    requestAnimationFrame(() => textareaRef.current?.focus());
  }, [past, future]);

  const redo = useCallback(() => {
    if (future.length === 0) return;
    const next = future[0];
    setFuture(future.slice(1));
    setPast((p) => [...p.slice(-99), contentRef.current]);
    setContent(next);
    requestAnimationFrame(() => textareaRef.current?.focus());
  }, [future]);

  const save = async () => {
    if (!selectedPath || !dirty) return;
    setSaving(true);
    setMessage(null);
    try {
      await api.writeTextFile(selectedPath, content);
      setOriginal(content);
      setMessage({ kind: "ok", text: "Saved" });
      onSave?.(selectedPath, content);
    } catch (e) {
      setMessage({ kind: "err", text: e instanceof Error ? e.message : String(e) });
    } finally {
      setSaving(false);
    }
  };

  const saveRef = useRef(save);
  saveRef.current = save;

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!paneRef.current?.contains(document.activeElement)) return;
      const mod = e.ctrlKey || e.metaKey;
      if (!mod) return;
      const k = e.key.toLowerCase();
      if (k === "s") {
        e.preventDefault();
        void saveRef.current();
      } else if (k === "z" && e.shiftKey) {
        e.preventDefault();
        redo();
      } else if (k === "z") {
        e.preventDefault();
        undo();
      } else if (k === "y") {
        e.preventDefault();
        redo();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [undo, redo]);

  const onEditorScroll = () => {
    const t = textareaRef.current;
    const layer = layerRef.current;
    if (t && layer) {
      layer.scrollTop = t.scrollTop;
      layer.scrollLeft = t.scrollLeft;
    }
  };

  const html = useMemo(
    () => (selectedPath ? highlight(content, languageFor(selectedPath)) : ""),
    [content, selectedPath],
  );

  if (!selectedPath) {
    return (
      <div className="editor-main-panel">
        <div className="editor-placeholder">Select a file to edit, or open one from a chat code block.</div>
      </div>
    );
  }

  const relativePath = (p: string) =>
    projectPath ? p.replace(projectPath.replaceAll("\\", "/") + "/", "") : p;

  return (
    <div className="editor-main-panel" ref={paneRef}>
      <div className="editor-main-container">
        <div className="editor-main-header">
          <button
            type="button"
            className="editor-icon-btn"
            onClick={undo}
            disabled={past.length === 0}
            title="Undo (Ctrl+Z)"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M9 14 4 9l5-5" />
              <path d="M4 9h10.5a5.5 5.5 0 0 1 5.5 5.5v0a5.5 5.5 0 0 1-5.5 5.5H11" />
            </svg>
          </button>
          <button
            type="button"
            className="editor-icon-btn"
            onClick={redo}
            disabled={future.length === 0}
            title="Redo (Ctrl+Shift+Z)"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="m15 14 5-5-5-5" />
              <path d="M20 9H9.5A5.5 5.5 0 0 0 4 14.5v0A5.5 5.5 0 0 0 9.5 20H13" />
            </svg>
          </button>
          <span className="editor-main-filename" title={selectedPath}>
            {selectedPath.split(/[/\\]/).pop()}
          </span>
          <span className="editor-main-path">{selectedPath}</span>
          {message && <span className={`editor-msg ${message.kind}`}>{message.text}</span>}
          {onAddContext && (
            <button type="button" className="btn-index" onClick={() => onAddContext(relativePath(selectedPath))} title="Add this file to the chat context">
              + Context
            </button>
          )}
          <button type="button" className="btn-index editor-save" onClick={() => void save()} disabled={!dirty || saving}>
            {saving ? "Saving…" : dirty ? "Save (Ctrl+S)" : "Saved"}
          </button>
        </div>
        <div className="editor-code-wrap">
          <div className="editor-layer" ref={layerRef} aria-hidden="true">
            <div className="editor-pre" dangerouslySetInnerHTML={{ __html: html }} />
          </div>
          <textarea
            ref={textareaRef}
            className="editor-textarea"
            value={content}
            onChange={(e) => commitChange(e.target.value)}
            onScroll={onEditorScroll}
            onSelect={refreshSelection}
            onKeyUp={refreshSelection}
            onMouseUp={refreshSelection}
            onBlur={refreshSelection}
            spellCheck={false}
            wrap="off"
          />
          {selection && (
            <button
              type="button"
              className="editor-ask-ai"
              onMouseDown={(e) => e.preventDefault()}
              onClick={askAI}
              title="Ask the coding agent about the selected code"
            >
              <span className="editor-ask-ai-icon" aria-hidden="true">✦</span>
              Ask AI
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

export default EditorPane;