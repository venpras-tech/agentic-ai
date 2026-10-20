import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { listen } from "@tauri-apps/api/event";
import { useChat, useContextPane } from "../../context/AppContext";
import { api, isTauriRuntime, type FileCheckpoint, type LspDiagnostic, type LspDiagnostics } from "../../lib/ipc";
import { highlight, languageFor } from "./highlight";
import "./editor.css";

interface EditorPaneProps {
  selectedPath: string | null;
  projectPath?: string | null;
  onAddContext?: (relativePath: string) => void;
  onSave?: (path: string, content: string) => void;
  /** 1-based line to scroll into view when the file loads (one-shot per value change). */
  focusLine?: number | null;
}

function EditorPane({ selectedPath, projectPath, onAddContext, onSave, focusLine }: EditorPaneProps) {
  const [content, setContent] = useState("");
  const [original, setOriginal] = useState("");
  const [saving, setSaving] = useState(false);
  const [message, setMessage] = useState<{ kind: "ok" | "err"; text: string } | null>(null);
  const [past, setPast] = useState<string[]>([]);
  const [future, setFuture] = useState<string[]>([]);
  const [selection, setSelection] = useState("");
  const [selRange, setSelRange] = useState<{ start: number; end: number } | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [history, setHistory] = useState<FileCheckpoint[]>([]);
  const [restoring, setRestoring] = useState(false);
  const [diags, setDiags] = useState<LspDiagnostics | null>(null);
  const [diagsOpen, setDiagsOpen] = useState(false);
  const diagReqRef = useRef(0);
  const navigate = useNavigate();
  const { setComposerDraft } = useChat();
  const { setActiveFile } = useContextPane();

  const paneRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const layerRef = useRef<HTMLDivElement>(null);
  const gutterRef = useRef<HTMLDivElement>(null);
  const contentRef = useRef(content);
  contentRef.current = content;
  const originalRef = useRef(original);
  originalRef.current = original;

  const dirty = selectedPath != null && content !== original;

  const refreshDiagnostics = useCallback(async (path: string) => {
    if (!isTauriRuntime()) return;
    const req = ++diagReqRef.current;
    try {
      const res = await api.lspDiagnostics(path);
      if (req !== diagReqRef.current) return;
      setDiags(res);
    } catch {
      if (req !== diagReqRef.current) return;
      setDiags(null);
    }
  }, []);

  const openFile = useCallback(async (path: string) => {
    setMessage(null);
    try {
      const result = await api.readTextFile(path);
      setContent(result.content);
      setOriginal(result.content);
      setPast([]);
      setFuture([]);
      void refreshDiagnostics(path);
    } catch (e) {
      setContent("");
      setOriginal("");
      setPast([]);
      setFuture([]);
      setDiags(null);
      setMessage({ kind: "err", text: e instanceof Error ? e.message : String(e) });
    }
  }, [refreshDiagnostics]);

  useEffect(() => {
    if (selectedPath) void openFile(selectedPath);
  }, [selectedPath, openFile]);

  // One-shot scroll to a requested line (e.g. "open file at hunk" from the
  // edit review panel). Retries briefly so it wins the race with the async
  // file read, then re-arms only when the target line actually changes.
  useEffect(() => {
    if (!selectedPath || !focusLine) return;
    const line = focusLine;
    let attempts = 0;
    let timer = 0;
    const tryScroll = () => {
      const t = textareaRef.current;
      if (t) {
        const lh = parseFloat(getComputedStyle(t).lineHeight);
        const lineH = Number.isFinite(lh) && lh > 0 ? lh : 21.7;
        t.scrollTop = Math.max(0, (line - 1) * lineH - t.clientHeight * 0.25);
        t.focus({ preventScroll: true });
        return;
      }
      if (attempts++ < 40) timer = window.setTimeout(tryScroll, 50);
    };
    requestAnimationFrame(tryScroll);
    return () => window.clearTimeout(timer);
  }, [selectedPath, focusLine]);

  // When the agent applies edits to the file currently open, pull the new
  // content in. Local, unsaved text is kept rather than clobbered.
  useEffect(() => {
    if (!isTauriRuntime() || !selectedPath) return;
    let unlisten: (() => void) | null = null;
    let disposed = false;
    const projectPrefix = (projectPath || "").replaceAll("\\", "/").replace(/\/+$/, "");
    const normalize = (p: string) => {
      const n = p.replaceAll("\\", "/").replace(/\/+$/, "");
      if (/^[A-Za-z]:/.test(n) || n.startsWith("/")) return n.toLowerCase();
      return `${projectPrefix}/${n}`.toLowerCase();
    };
    const refresh = (payload: unknown) => {
      if (disposed || !selectedPath) return;
      const edits = (payload as { edits?: Array<{ path?: string; status?: string }> }).edits ?? [];
      const applied = edits.filter((e) => e.status === "applied");
      if (applied.length === 0) return;
      const target = normalize(selectedPath);
      const hit = applied.some((e) => normalize(e.path ?? "") === target);
      if (!hit) return;
      if (originalRef.current !== contentRef.current) {
        setMessage({
          kind: "ok",
          text: "This file was updated by the agent on disk — local unsaved changes were kept.",
        });
        void refreshDiagnostics(selectedPath);
        return;
      }
      void openFile(selectedPath).then(() => {
        setMessage({ kind: "ok", text: "File updated by the agent — reloaded from disk" });
      });
    };
    void listen("agent://edit-applied", (e) => refresh(e.payload)).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    let unlistenWorkspace: (() => void) | null = null;
    const refreshWorkspace = (payload: unknown) => {
      if (disposed || !selectedPath) return;
      const paths = (payload as { paths?: string[] }).paths ?? [];
      const target = normalize(selectedPath);
      if (!paths.some((p) => normalize(p) === target)) return;
      if (originalRef.current !== contentRef.current) {
        setMessage({
          kind: "ok",
          text: "This file changed on disk — local unsaved changes were kept.",
        });
        void refreshDiagnostics(selectedPath);
        return;
      }
      void openFile(selectedPath).then(() => {
        setMessage({ kind: "ok", text: "File changed on disk — reloaded" });
      });
    };
    void listen("workspace://file-changed", (e) => refreshWorkspace(e.payload)).then((fn) => {
      if (disposed) fn();
      else unlistenWorkspace = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
      unlistenWorkspace?.();
    };
  }, [selectedPath, projectPath, openFile, refreshDiagnostics]);

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
      setSelRange(null);
      return;
    }
    const has = el.selectionEnd > el.selectionStart;
    const s = el.value.slice(el.selectionStart, el.selectionEnd);
    setSelection(has ? s : "");
    setSelRange(has ? { start: el.selectionStart, end: el.selectionEnd } : null);
  };

  const selectionLines = (): { startLine: number; endLine: number } | null => {
    if (!selRange) return null;
    const lineAt = (offset: number) =>
      contentRef.current.slice(0, Math.min(offset, contentRef.current.length)).split("\n").length;
    return { startLine: lineAt(selRange.start), endLine: lineAt(selRange.end) };
  };

  const pinSelection = (sel: string) => {
    if (!selectedPath || !isTauriRuntime()) return;
    const lines = selectionLines();
    void api.contextSetSelection({
      path: selectedPath,
      startLine: lines?.startLine ?? null,
      endLine: lines?.endLine ?? null,
      content: sel,
    });
  };

  const addContext = () => {
    if (!selectedPath) return;
    const sel = selection.trim();
    if (sel) {
      pinSelection(sel);
      setSelection("");
      setSelRange(null);
    }
    onAddContext?.(relativePath(selectedPath));
  };

  const askAI = () => {
    const sel = selection.trim();
    if (!sel) return;
    pinSelection(sel);
    const prompt = `About this code: ${sel}`;
    setComposerDraft(prompt);
    window.dispatchEvent(new CustomEvent("vencoder-prefill-composer", { detail: prompt }));
    setSelection("");
    setSelRange(null);
    navigate("/chat");
  };

  const sendPrompt = (prompt: string) => {
    setComposerDraft(prompt);
    window.dispatchEvent(new CustomEvent("vencoder-prefill-composer", { detail: prompt }));
    navigate("/chat");
  };

  const explainDiagnostic = (d: LspDiagnostic) => {
    const rel = relativePath(d.path);
    sendPrompt(
      `Explain this LSP ${d.severity} in ${rel}:${d.line} — "${d.message}". What causes it and how should it be fixed?`,
    );
  };

  const fixDiagnostics = () => {
    if (!selectedPath || !diags || diags.diagnostics.length === 0) return;
    const rel = relativePath(selectedPath);
    const items = diags.diagnostics
      .map((d) => `- ${d.severity} (${d.path}:${d.line}:${d.column}): ${d.message}`)
      .join("\n");
    sendPrompt(
      `Fix the LSP diagnostics reported for ${rel}:\n${items}\n\nRead the file, fix each issue with a minimal edit, then confirm the diagnostics clear.`,
    );
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
      void refreshDiagnostics(selectedPath);
    } catch (e) {
      setMessage({ kind: "err", text: e instanceof Error ? e.message : String(e) });
    } finally {
      setSaving(false);
    }
  };

  const saveRef = useRef(save);
  saveRef.current = save;

  const toggleHistory = async () => {
    if (!historyOpen) {
      try {
        const all = await api.checkpointList();
        const hs = normalizeAll(selectedPath);
        setHistory(
          all
            .filter((c) => hs.includes(c.path))
            .sort((a, b) => b.timestampMs - a.timestampMs)
        );
      } catch {
        setHistory([]);
      }
    }
    setHistoryOpen((v) => !v);
  };

  const normalizeAll = (p: string | null) => {
    if (!p) return [];
    const projectPrefix = (projectPath || "").replaceAll("\\", "/").replace(/\/+$/, "");
    const n = p.replaceAll("\\", "/").replace(/\/+$/, "");
    if (/^[A-Za-z]:/.test(n) || n.startsWith("/")) return [n.toLowerCase()];
    return [`${projectPrefix}/${n}`.toLowerCase()];
  };

  const restoreCheckpoint = async (c: FileCheckpoint) => {
    if (!selectedPath) return;
    setRestoring(true);
    setMessage(null);
    try {
      await api.checkpointRestore([c.id]);
      await openFile(selectedPath);
      setMessage({ kind: "ok", text: `Restored snapshot from ${new Date(c.timestampMs).toLocaleString()}` });
      await toggleHistory();
    } catch (e) {
      setMessage({ kind: "err", text: e instanceof Error ? e.message : String(e) });
    } finally {
      setRestoring(false);
    }
  };

  // Agent edits land on disk as `agent://edit-applied`, but the editor only
  // reloads when the user navigates back to the file. Follow the live edits:
  // reload the open file whenever an applied batch touches it (and skip the
  // reload while the buffer has unsaved user changes to avoid clobbering them).
  const dirtyRef = useRef(dirty);
  dirtyRef.current = dirty;
  const selectedPathRef = useRef(selectedPath);
  selectedPathRef.current = selectedPath;
  const projectRef = useRef(projectPath);
  projectRef.current = projectPath;
  const openFileRef = useRef(openFile);
  openFileRef.current = openFile;
  const sameFile = useRef((a: string, b: string) => {
    const norm = (p: string) => {
      const n = (p || "").replaceAll("\\", "/").replace(/\/+$/, "");
      if (/^[A-Za-z]:/.test(n) || n.startsWith("/")) return n.toLowerCase();
      const prefix = (projectRef.current || "").replaceAll("\\", "/").replace(/\/+$/, "");
      return `${prefix}/${n}`.toLowerCase();
    };
    return norm(a) === norm(b);
  }).current;
  useEffect(() => {
    const onApplied = (e: Event) => {
      const detail = (e as CustomEvent<{ edits?: Array<{ path?: string; status?: string }> }>).detail;
      const path = selectedPathRef.current;
      if (!path || dirtyRef.current) return;
      const hit = (detail?.edits ?? []).some(
        (ed) => ed.status === "applied" && ed.path && sameFile(path, ed.path),
      );
      if (hit) void openFileRef.current(path);
    };
    window.addEventListener("vencoder-edit-applied", onApplied);
    return () => window.removeEventListener("vencoder-edit-applied", onApplied);
  }, [sameFile]);

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
    const gutter = gutterRef.current;
    if (t && layer) {
      layer.scrollTop = t.scrollTop;
      layer.scrollLeft = t.scrollLeft;
    }
    if (t && gutter) {
      gutter.scrollTop = t.scrollTop;
    }
  };

  const html = useMemo(
    () => (selectedPath ? highlight(content, languageFor(selectedPath)) : ""),
    [content, selectedPath],
  );

  const diagCount = diags?.diagnostics.length ?? 0;
  const errCount = diags?.diagnostics.filter((d) => d.severity === "error").length ?? 0;
  const warnCount = diags?.diagnostics.filter((d) => d.severity === "warning").length ?? 0;
  const diagTitle = diagCount === 0 ? "No LSP diagnostics" : diags!.diagnostics.map((d) => `:${d.line} ${d.severity}: ${d.message}`).join("\n");

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
            aria-label="Undo"
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
            aria-label="Redo"
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
            <button type="button" className="btn-index" onClick={addContext} title="Add this file to the chat context">
              + Context
            </button>
          )}
          <button
            type="button"
            className="btn-index"
            onClick={() => void toggleHistory()}
            title="Snapshots the agent took before applying edits to this file"
          >
            History
          </button>
          {isTauriRuntime() && (
            <>
              <button
                type="button"
                className={`btn-index ${diags?.server ? "" : "editor-diag-off"}`}
                onClick={() => setDiagsOpen((v) => !v)}
                disabled={!diags?.server}
                title={diags?.server ? diagTitle : "No language server for this file (LSP off — see Settings)"}
              >
                {diagCount === 0 ? "Diagnostics" : `Diagnostics · ${errCount}/${warnCount}`}
              </button>
              <button
                type="button"
                className="btn-index editor-fix"
                onClick={fixDiagnostics}
                disabled={diagCount === 0}
                title="Ask the coding agent to fix the reported diagnostics"
              >
                Fix diagnostics
              </button>
            </>
          )}
          <button type="button" className="btn-index editor-save" onClick={() => void save()} disabled={!dirty || saving}>
            {saving ? "Saving…" : dirty ? "Save (Ctrl+S)" : "Saved"}
          </button>
        </div>
        {historyOpen && (
          <div className="editor-history-panel">
            <div className="editor-history-head">
              <span>Agent edit snapshots</span>
              <button type="button" className="editor-history-close" onClick={() => setHistoryOpen(false)}>
                ×
              </button>
            </div>
            {history.length === 0 ? (
              <div className="editor-history-empty">
                No snapshots for this file yet — they appear whenever the agent proposes an edit here.
              </div>
            ) : (
              <ul className="editor-history-list">
                {history.map((c) => (
                  <li key={c.id} className="editor-history-item">
                    <span className="editor-history-meta">
                      {new Date(c.timestampMs).toLocaleString()}
                      <code>{c.editId}</code>
                    </span>
                    <button
                      type="button"
                      className="editor-history-restore"
                      onClick={() => void restoreCheckpoint(c)}
                      disabled={restoring}
                    >
                      {restoring ? "…" : "Restore"}
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
        {diagsOpen && diags && diags.server && (
          <div className="editor-diag-panel">
            <div className="editor-diag-head">
              <span>
                LSP diagnostics · {diags.server}
                {dirty && <em className="editor-diag-dirty"> (reflects saved file)</em>}
              </span>
              <button type="button" className="editor-diag-close" onClick={() => setDiagsOpen(false)}>
                ×
              </button>
            </div>
            {diagCount === 0 ? (
              <div className="editor-diag-empty">No issues reported.</div>
            ) : (
              <ul className="editor-diag-list">
                {diags.diagnostics.map((d, i) => (
                  <li key={`${d.line}-${d.column}-${d.severity}-${i}`} className="editor-diag-item">
                    <button
                      type="button"
                      className="editor-diag-row"
                      onClick={() => explainDiagnostic(d)}
                      title="Ask the coding agent to explain this diagnostic"
                    >
                      <span className={`editor-diag-dot editor-diag-${d.severity}`} aria-hidden="true" />
                      <span className="editor-diag-loc">
                        {d.path.split(/[/\\]/).pop()}:{d.line}:{d.column}
                      </span>
                      <span className="editor-diag-msg">{d.message}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
        <div className="editor-code-wrap">
          <div className="editor-layer" ref={layerRef} aria-hidden="true">
            <div className="editor-pre" dangerouslySetInnerHTML={{ __html: html }} />
          </div>
          {diags && diagCount > 0 && (
            <div className="editor-gutter" ref={gutterRef}>
              {diags.diagnostics.map((d, i) => (
                <button
                  key={`${d.line}-${d.column}-${d.severity}-${i}`}
                  type="button"
                  className={`editor-gutter-marker editor-gutter-${d.severity}`}
                  style={{ top: `${14 + (d.line - 1) * 21.7}px` }}
                  title={`${d.severity}: ${d.message}`}
                  onMouseDown={(e) => e.preventDefault()}
                  onClick={(e) => {
                    e.stopPropagation();
                    explainDiagnostic(d);
                  }}
                />
              ))}
            </div>
          )}
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