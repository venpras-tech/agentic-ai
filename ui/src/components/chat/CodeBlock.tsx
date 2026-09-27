import { useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { api } from "../../lib/ipc";
import "./chat.css";

function codeText(children: ReactNode): string {
  let out = "";
  const walk = (n: unknown): void => {
    if (n == null || n === false) return;
    if (typeof n === "string" || typeof n === "number") {
      out += String(n);
      return;
    }
    if (Array.isArray(n)) {
      n.forEach(walk);
      return;
    }
    if (typeof n === "object") {
      const o = n as { props?: { children?: unknown } };
      if (o.props && typeof o.props === "object") walk(o.props.children);
    }
  };
  walk(children);
  return out;
}

function CodeBlock({
  className,
  children,
  inline,
}: {
  className?: string;
  children?: ReactNode;
  inline?: boolean;
}) {
  const code = codeText(children);
  const lang = (className || "").replace(/^language-/, "").trim();
  const pathLike = Boolean(lang) && (/[/\\]/.test(lang) || /\.[a-zA-Z0-9]+$/.test(lang)) && !/\s/.test(lang);

  const [copied, setCopied] = useState(false);
  const [pathMode, setPathMode] = useState(false);
  const [path, setPath] = useState(pathLike ? lang : "");
  const [applying, setApplying] = useState(false);
  const [applied, setApplied] = useState<null | "ok" | "err">(null);
  const [err, setErr] = useState("");
  const timer = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (timer.current != null) window.clearTimeout(timer.current);
    };
  }, []);

  const flash = (which: "ok" | "err" | "copied", msg?: string) => {
    if (which === "copied") setCopied(true);
    else {
      setApplied(which);
      setErr(msg || "");
    }
    if (timer.current != null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      setCopied(false);
      setApplied(null);
      setErr("");
    }, 1800);
  };

  if (inline) {
    return <code className={className}>{children}</code>;
  }

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      flash("copied");
    } catch {
      // clipboard denied — nothing sensible to do
    }
  };

  const apply = async () => {
    const target = path.trim();
    if (!target) return;
    setApplying(true);
    setApplied(null);
    try {
      await api.writeTextFile(target, code);
      flash("ok");
    } catch (e) {
      flash("err", e instanceof Error ? e.message : String(e));
    } finally {
      setApplying(false);
    }
  };

  const openInEditor = async () => {
    const target = path.trim();
    if (!target) return;
    try {
      await api.writeTextFile(target, code);
      window.dispatchEvent(new CustomEvent("vencoder-open-editor", { detail: target }));
    } catch {
      // fall through silently — write failed
    }
  };

  return (
    <div className={`chat-code-block${lang ? ` lang-${lang.replace(/[^a-zA-Z0-9_-]/g, "")}` : ""}`}>
      <div className="chat-code-actions">
        <span className="chat-code-lang">{lang || "code"}</span>
        <button type="button" className="chat-code-action" onClick={() => void copy()}>
          {copied ? "copied ✓" : "Copy"}
        </button>
        <button
          type="button"
          className={`chat-code-action${applied === "ok" ? " ok" : applied === "err" ? " err" : ""}`}
          onClick={() => setPathMode((v) => !v)}
          title="Apply the code block to a file"
        >
          {applied === "ok" ? "✓ Applied" : applied === "err" ? (err || "Failed") : "Apply to File"}
        </button>
        <button
          type="button"
          className="chat-code-action"
          onClick={() => void openInEditor()}
          title="Write to file and open it"
          disabled={!path.trim()}
        >
          Open in Editor
        </button>
      </div>
      {pathMode && (
        <div className="chat-code-path">
          <input
            type="text"
            value={path}
            onChange={(e) => {
              setPath(e.target.value);
              setApplied(null);
            }}
            placeholder="path/to/file.ext"
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void apply();
              } else if (e.key === "Escape") {
                setPathMode(false);
              }
            }}
            autoFocus
          />
          <button
            type="button"
            className="chat-code-action"
            onClick={() => void apply()}
            disabled={!path.trim() || applying}
          >
            {applying ? "Applying…" : "Apply"}
          </button>
        </div>
      )}
      <pre className="chat-code-pre">
        <code className={className}>{children}</code>
      </pre>
    </div>
  );
}

export default CodeBlock;