import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { api } from "../lib/ipc";

interface DiffLine {
  tag: "header" | "add" | "del" | "ctx" | "meta";
  text: string;
}

function parseUnifiedDiff(raw: string): DiffLine[] {
  const lines = raw.split(/\r?\n/);
  const out: DiffLine[] = [];
  for (const line of lines) {
    if (line.startsWith("+++") || line.startsWith("---")) {
      out.push({ tag: "meta", text: line });
    } else if (line.startsWith("@@")) {
      out.push({ tag: "header", text: line });
    } else if (line.startsWith("+")) {
      out.push({ tag: "add", text: line });
    } else if (line.startsWith("-")) {
      out.push({ tag: "del", text: line });
    } else {
      out.push({ tag: "ctx", text: line });
    }
  }
  return out;
}

const TAG_CLASS: Record<DiffLine["tag"], string> = {
  header: "bg-cyan-500/10 text-cyan-600/90",
  add: "bg-emerald-500/10",
  del: "bg-red-500/10",
  ctx: "",
  meta: "text-zinc-400",
};

const ROW_HEIGHT = 16;
const OVERSCAN = 30;

type DiffStatus = "pending" | "accepted" | "rejected";

interface DiffViewProps {
  path: string;
  diff: string;
  /** Pre-change content for revert. */
  before?: string;
  /** Controlled resolution state; when set, the view reflects it. */
  resolved?: DiffStatus;
  /** Called when the diff is accepted or rejected. */
  onResolved?: (accepted: boolean) => void;
}

// ---------------------------------------------------------------------------
// Lightweight, dependency-free syntax highlighting for diff content lines.
// Colors code tokens inside added/removed/context lines while leaving the
// unified-diff prefix (`+`/`-`/` `) muted. Mono-ish keyword sets, intentionally
// small so it never slows the windowed renderer.
// ---------------------------------------------------------------------------
type Token = { text: string; cls: string | null; key: string };
const KEYWORDS = new Set(
  "fn function def class struct enum impl trait pub const let var mut return if else for while match break continue async await import from export type interface extends implements new try catch throw self super static" +
    " int main void this std println print echo import def end if end else true false None nil".split(" "),
);
const KEYWORD_CLS = "text-cyan-700/90";
const STRING_CLS = "text-amber-700/90";
const COMMENT_CLS = "text-zinc-400 italic";
const NUMBER_CLS = "text-blue-700/90";
const PLAIN_CLS = "text-zinc-700";

/** Heuristic language tag from the diff path (drives the tokenizer only). */
function langFromPath(path: string): "js" | "py" | "rs" | "other" {
  const p = path.toLowerCase().split(/[\\/]/).pop() ?? "";
  if (/\.(js|jsx|ts|tsx|mjs|cjs)$/.test(p)) return "js";
  if (/\.py$/.test(p)) return "py";
  if (/\.rs$/.test(p)) return "rs";
  return "other";
}

function startsComment(text: string, lang: "js" | "py" | "rs" | "other"): boolean {
  const trimmed = text.trimStart();
  if (trimmed.startsWith("/*")) return true;
  if (lang === "py") return trimmed.startsWith("#") || trimmed.startsWith('"""') || trimmed.startsWith("'''");
  if (trimmed.startsWith("//") || trimmed.startsWith("--")) return true;
  return false;
}

/** Tokenize one diff content line (after its `+/-/ ` prefix). */
function tokenizeContent(text: string): Token[] {
  const tokens: Token[] = [];
  let key = 0;
  let rest = text;
  const push = (piece: string, cls: string | null) => {
    if (piece) tokens.push({ text: piece, cls, key: `t${key++}` });
  };

  while (rest.length > 0) {
    const match = rest.match(/^(\s+)/);
    if (match) {
      push(match[1], null);
      rest = rest.slice(match[1].length);
      continue;
    }
    const id = rest.match(/^[A-Za-z_$][A-Za-z0-9_$]*/);
    if (id) {
      const word = id[0];
      push(word, KEYWORDS.has(word) ? KEYWORD_CLS : PLAIN_CLS);
      rest = rest.slice(word.length);
      continue;
    }
    const num = rest.match(/^\d+(?:\.\d+)?/);
    if (num) {
      push(num[0], NUMBER_CLS);
      rest = rest.slice(num[0].length);
      continue;
    }
    const str = rest.match(/^("(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`)/);
    if (str) {
      push(str[1], STRING_CLS);
      rest = rest.slice(str[1].length);
      continue;
    }
    push(rest[0], null);
    rest = rest.slice(1);
  }
  return tokens;
}

/** Render a diff line: muted prefix + highlighted content for code lines. */
function DiffText({ text, lang }: { text: string; lang: "js" | "py" | "rs" | "other" }) {
  const prefix = text[0];
  const body = text.slice(1);
  const comment = startsComment(body.trimStart(), lang);

  if (comment) {
    return (
      <span>
        <span className="text-zinc-400">{prefix}</span>
        <span className={COMMENT_CLS}>{body}</span>
      </span>
    );
  }
  const tokens = tokenizeContent(body);
  return (
    <span>
      <span className="text-zinc-400">{prefix}</span>
      {tokens.map((t) =>
        t.cls ? (
          <span key={t.key} className={t.cls}>
            {t.text}
          </span>
        ) : (
          <span key={t.key}>{t.text}</span>
        ),
      )}
    </span>
  );
}

export default function DiffView({ path, diff, before, resolved, onResolved }: DiffViewProps) {
  const [status, setStatus] = useState<"pending" | "accepted" | "rejected">(
    resolved ?? "pending",
  );
  const [revertError, setRevertError] = useState<string | null>(null);
  const { lines, adds, dels } = useMemo(() => {
    const parsed = parseUnifiedDiff(diff);
    let a = 0;
    let d = 0;
    for (const l of parsed) {
      if (l.tag === "add") a++;
      else if (l.tag === "del") d++;
    }
    return { lines: parsed, adds: a, dels: d };
  }, [diff]);
  const short = path.split(/[\\/]/).pop() ?? path;
  const lang = useMemo(() => langFromPath(path), [path]);

  // Reflect externally-driven resolution (e.g. the Changes panel bulk actions)
  // without losing local Accept/Reject state too.
  useEffect(() => {
    if (resolved != null) setStatus(resolved);
  }, [resolved]);

  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [start, setStart] = useState(0);
  const [end, setEnd] = useState(50);

  const updateWindow = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const first = Math.max(0, Math.floor(el.scrollTop / ROW_HEIGHT) - OVERSCAN);
    const visible = Math.ceil(el.clientHeight / ROW_HEIGHT) + OVERSCAN * 2;
    setStart(first);
    setEnd(first + visible);
  }, []);

  useLayoutEffect(() => {
    updateWindow();
  }, [lines.length, updateWindow]);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const onScroll = () => updateWindow();
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, [updateWindow]);

  const handleAccept = () => {
    if (status !== "pending") return;
    setStatus("accepted");
    onResolved?.(true);
  };

  const handleReject = async () => {
    if (status !== "pending" || !before) return;
    setRevertError(null);
    try {
      await api.revertFile(path, before);
      setStatus("rejected");
      onResolved?.(false);
    } catch (e) {
      setRevertError(String(e));
    }
  };

  return (
    <div className="rounded-md border border-border bg-zinc-100">
      <div className="flex items-center justify-between gap-2 border-b border-border px-2 py-1">
        <span className="truncate font-mono text-[10px] text-zinc-500">{short}</span>
        <span className="flex items-center gap-1.5">
          <span className="shrink-0 text-[9px] tabular-nums text-zinc-400">
            <span className="text-emerald-500">+{adds}</span>{" "}
            <span className="text-red-500">-{dels}</span>
          </span>
          {status === "pending" && (
            <>
              <button
                type="button"
                onClick={handleAccept}
                aria-label={`Accept changes to ${short}`}
                className="rounded bg-emerald-500/10 px-1.5 py-0.5 text-[9px] font-medium text-emerald-600 hover:bg-emerald-500/20"
              >
                Accept
              </button>
              {before && (
                <button
                  type="button"
                  onClick={handleReject}
                  aria-label={`Revert changes to ${short}`}
                  className="rounded bg-red-500/10 px-1.5 py-0.5 text-[9px] font-medium text-red-600 hover:bg-red-500/20"
                >
                  Reject
                </button>
              )}
            </>
          )}
          {status === "accepted" && (
            <span className="text-[9px] font-medium text-emerald-600">Accepted</span>
          )}
          {status === "rejected" && (
            <span className="text-[9px] font-medium text-red-600">Reverted</span>
          )}
        </span>
      </div>
      {revertError && (
        <div
          role="alert"
          className="border-b border-red-500/30 bg-red-500/10 px-2 py-1 text-[9px] text-red-600"
        >
          Revert failed: {revertError}
        </div>
      )}
      <div
        ref={scrollRef}
        onScroll={updateWindow}
        className="max-h-64 overflow-auto py-1 font-mono text-[10px] leading-snug"
      >
        <div style={{ height: lines.length * ROW_HEIGHT, position: "relative" }}>
          {lines.slice(start, end).map((l, i) => (
            <div
              key={start + i}
              className={`whitespace-pre-wrap px-2 ${TAG_CLASS[l.tag]} ${
                l.tag === "add"
                  ? "text-emerald-700/90"
                  : l.tag === "del"
                    ? "text-red-600/90"
                    : l.tag === "ctx"
                      ? "text-zinc-600"
                      : ""
              }`}
              style={{ position: "absolute", top: (start + i) * ROW_HEIGHT, left: 0, right: 0 }}
            >
              {l.tag === "add" || l.tag === "del" ? (
                <DiffText text={l.text} lang={lang} />
              ) : (
                l.text || " "
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
