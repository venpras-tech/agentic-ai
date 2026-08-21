import Editor from "@monaco-editor/react";

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
        language={languageFromPath(file.path)}
        value={file.content}
        onChange={(value) => onContentChange(value ?? "")}
        theme="vs"
        options={{
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
        }}
      />
    </div>
  );
}
