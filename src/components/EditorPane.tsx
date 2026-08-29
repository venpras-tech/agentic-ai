import { useEffect, useRef } from "react";
import Editor, { type OnMount } from "@monaco-editor/react";
import * as monaco from "monaco-editor";

import "../lib/monaco";
import type { OpenFile } from "../types";

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

interface EditorPaneProps {
  file: OpenFile | null;
  onContentChange: (content: string) => void;
}

export default function EditorPane({ file, onContentChange }: EditorPaneProps) {
  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);
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
    editorRef.current = editor;
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
    </div>
  );
}
