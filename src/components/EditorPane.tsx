import { useEffect, useRef, useState } from "react";
import Editor, { type OnMount } from "@monaco-editor/react";
import * as monaco from "monaco-editor";

import "../lib/monaco";
import type { OpenFile } from "../types";
import { api } from "../lib/ipc";
import HunkDecorations, { type PendingDiffSection } from "./HunkDecorations";

const EXT_TO_LANG: Record<string, string> = {
  ts: "typescript",
  tsx: "typescript",
  js: "javascript",
  jsx: "javascript",
  mjs: "javascript",
  rs: "rust",
  py: "python",
  go: "go",
  c: "c",
  h: "c",
  cpp: "cpp",
  hpp: "cpp",
  cc: "cpp",
  cs: "csharp",
  json: "json",
  md: "markdown",
  html: "html",
  css: "css",
  scss: "scss",
  toml: "ini",
  yml: "yaml",
  yaml: "yaml",
  sh: "shell",
  bash: "shell",
  sql: "sql",
  java: "java",
  kt: "kotlin",
  rb: "ruby",
  php: "php",
  xml: "xml",
  vue: "vue",
};

const EDITOR_OPTIONS: monaco.editor.IStandaloneEditorConstructionOptions = {
  fontSize: 13,
  fontFamily:
    "'Cascadia Code', 'JetBrains Mono', Consolas, 'Courier New', monospace",
  fontLigatures: true,
  minimap: { enabled: true, maxColumn: 80 },
  scrollBeyondLastLine: false,
  smoothScrolling: true,
  cursorBlinking: "smooth",
  automaticLayout: true,
  tabSize: 2,
  wordWrap: "off",
  renderWhitespace: "selection",
  padding: { top: 10 },
  stickyScroll: { enabled: false },
  folding: true,
  bracketPairColorization: { enabled: true },
  guides: { bracketPairs: true, indentation: true },
  lineNumbersMinChars: 3,
  scrollbar: { verticalScrollbarSize: 10, horizontalScrollbarSize: 10 },
};

function languageFromPath(path: string | null): string {
  if (!path) return "plaintext";
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return EXT_TO_LANG[ext] ?? "plaintext";
}

// ---------------------------------------------------------------------------
// Inline AI completion (Monaco ghost text) — routes to the backend
// `autocomplete_generate` command, which prefers the `Autocomplete` provider
// role (falling back to the local pool). Wired but conservative: it only fires
// on real code lines and drops stale responses.
// ---------------------------------------------------------------------------
const COMPLETION_LANGS = [
  "typescript",
  "javascript",
  "rust",
  "python",
  "go",
  "c",
  "cpp",
  "csharp",
  "java",
  "kotlin",
  "ruby",
  "php",
  "sql",
  "shell",
  "json",
  "yaml",
  "css",
  "html",
];

const MIN_PREFIX_CHARS = 6;
const MIN_FETCH_INTERVAL_MS = 250;
let inlineProviderRegistered = false;
let lastFetchAt = 0;
let fetchSeq = 0;

function ensureInlineProvider() {
  if (inlineProviderRegistered) return;
  inlineProviderRegistered = true;

  monaco.languages.registerInlineCompletionsProvider(COMPLETION_LANGS, {
    provideInlineCompletions: async (model, position) => {
      // Skip when the caret is mid-word on a whitespace/comment line or when the
      // buffered prefix is too short to be meaningful.
      const lineText = model.getLineContent(position.lineNumber);
      const trimmedLine = lineText.trim();
      if (
        trimmedLine === "" ||
        trimmedLine.startsWith("//") ||
        trimmedLine.startsWith("#") ||
        trimmedLine.startsWith("/*") ||
        trimmedLine.startsWith("*")
      ) {
        return { items: [] };
      }
      const prefix = model.getValueInRange({
        startLineNumber: 1,
        startColumn: 1,
        endLineNumber: position.lineNumber,
        endColumn: position.column,
      });
      if (!prefix || prefix.trim().length < MIN_PREFIX_CHARS) {
        return { items: [] };
      }
      // Drop overly frequent calls (typing bursts) to avoid hammering the backend.
      const now = Date.now();
      if (now - lastFetchAt < MIN_FETCH_INTERVAL_MS) return { items: [] };
      lastFetchAt = now;

      const suffix = model.getValueInRange({
        startLineNumber: position.lineNumber,
        startColumn: position.column,
        endLineNumber: model.getLineCount(),
        endColumn: model.getLineMaxColumn(model.getLineCount()),
      });

      const seq = ++fetchSeq;
      let text: string;
      try {
        text = await api.autocomplete({
          prefix,
          suffix: suffix.trim() ? suffix : undefined,
          language: model.getLanguageId(),
          maxTokens: 128,
        });
      } catch {
        return { items: [] };
      }
      // Stale response (caret moved on) — drop it.
      if (seq !== fetchSeq) return { items: [] };
      const trimmed = text.trim();
      if (trimmed.length === 0 || trimmed.length > 200) return { items: [] };
      const range = new monaco.Range(
        position.lineNumber,
        position.column,
        position.lineNumber,
        position.column,
      );
      return { items: [{ insertText: trimmed, range }] };
    },
    freeInlineCompletions: () => {},
  });
}

interface EditorPaneProps {
  file: OpenFile | null;
  onContentChange: (content: string) => void;
  /**
   * Pending authored diffs (from agent file edits) that apply to this file.
   * Each carries the pre-change `before` text so individual hunks can be
   * toggled relative to a stable base.
   */
  pendingSections?: PendingDiffSection[];
  /** Hunk resolution map (hunk key -> kept) from the parent reducer. */
  hunkResolution?: Record<string, boolean> | null;
  /** Fired when the user keeps/discards an individual hunk. */
  onToggleHunk?: (opts: {
    before: string;
    diff: string | null;
    key: string;
    keep: boolean;
    content: string;
  }) => void;
}

export default function EditorPane({
  file,
  onContentChange,
  pendingSections,
  hunkResolution,
  onToggleHunk,
}: EditorPaneProps) {
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
  const [mountedEditor, setMountedEditor] = useState<monaco.editor.IStandaloneCodeEditor | null>(null);
  const modelsRef = useRef<Map<string, monaco.editor.ITextModel>>(new Map());
  const activeKeyRef = useRef<string | null>(null);
  const onChangeRef = useRef(onContentChange);
  onChangeRef.current = onContentChange;

  const key = file?.id ?? null;

  useEffect(() => {
    if (!file || !key) return;
    let model = modelsRef.current.get(key);
    if (!model) {
      model = monaco.editor.createModel(
        file.content,
        languageFromPath(file.path),
      );
      modelsRef.current.set(key, model);
    } else {
      monaco.editor.setModelLanguage(model, languageFromPath(file.path));
      if (model.getValue() !== file.content) {
        model.pushEditOperations(
          [],
          [{ range: model.getFullModelRange(), text: file.content }],
          () => null,
        );
      }
    }
    if (key !== activeKeyRef.current && editorRef.current) {
      editorRef.current.setModel(model);
      activeKeyRef.current = key;
    }
  }, [key, file]);

  const handleMount: OnMount = (editor) => {
    ensureInlineProvider();
    editorRef.current = editor;
    setMountedEditor(editor);
    if (key) {
      let model = modelsRef.current.get(key);
      if (!model) {
        model = editor.getModel() ?? monaco.editor.createModel(file?.content ?? "", languageFromPath(file?.path ?? null));
        modelsRef.current.set(key, model);
      }
      editor.setModel(model);
      activeKeyRef.current = key;
    }
  };

  if (!file) {
    return (
      <div className="flex h-full flex-col items-center justify-center gap-1 bg-editor">
        <span className="text-sm text-zinc-600">No file open</span>
        <span className="text-[11px] text-zinc-700">
          Open a workspace and a file, or hit + to start typing
        </span>
      </div>
    );
  }

  return (
    <div className="h-full w-full bg-editor">
      <Editor
        theme="vs"
        options={EDITOR_OPTIONS}
        onMount={handleMount}
        onChange={(value) => onChangeRef.current(value ?? "")}
      />
      {mountedEditor && pendingSections && pendingSections.length > 0 && (
        <HunkDecorations
          editor={mountedEditor}
          path={file.path}
          sections={pendingSections}
          resolution={hunkResolution ?? null}
          onToggleHunk={onToggleHunk ?? (() => {})}
        />
      )}
    </div>
  );
}
