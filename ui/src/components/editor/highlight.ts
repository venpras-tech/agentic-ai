type TokenClass = "tok-kw" | "tok-str" | "tok-num" | "tok-com" | "tok-fn" | "tok-tag" | null;

type LangFamily = "ts" | "rust" | "python" | "markup" | "shell" | "plain";

const KEYWORDS: Record<Exclude<LangFamily, "markup" | "plain">, string[]> = {
  ts: [
    "const", "let", "var", "function", "return", "if", "else", "for", "while",
    "do", "switch", "case", "default", "break", "continue", "class", "interface",
    "type", "extends", "implements", "import", "export", "from", "new", "delete",
    "async", "await", "try", "catch", "finally", "throw", "in", "of", "instanceof",
    "typeof", "void", "null", "undefined", "true", "false", "this", "super",
    "static", "readonly", "enum", "namespace", "get", "set", "yield", "as",
    "public", "private", "protected", "abstract",
  ],
  rust: [
    "fn", "let", "mut", "pub", "use", "mod", "struct", "enum", "impl", "trait",
    "match", "if", "else", "for", "while", "loop", "return", "move", "async",
    "await", "unsafe", "where", "const", "static", "type", "dyn", "self", "Self",
    "true", "false", "break", "continue", "ref", "in", "as",
  ],
  python: [
    "def", "class", "return", "if", "elif", "else", "for", "while", "import",
    "from", "as", "with", "try", "except", "finally", "raise", "pass", "break",
    "continue", "lambda", "yield", "global", "nonlocal", "del", "in", "is",
    "not", "and", "or", "None", "True", "False", "assert", "async", "await",
  ],
  shell: [
    "if", "then", "else", "elif", "fi", "for", "while", "do", "done", "case",
    "esac", "function", "return", "local", "export", "readonly", "in", "echo",
    "true", "false", "exit", "source", "shift", "set", "unset",
  ],
};

function escapeHtml(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

const STRING_RE = /"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|`(?:\\.|[^`\\])*`/.source;
const NUMBER_RE = /\b\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/.source;
const FN_RE = /\b[A-Za-z_][A-Za-z0-9_]*\b(?=\()/.source;
const WORD_RE = /\b[A-Za-z_][A-Za-z0-9_]*\b/.source;
const TAG_RE = /<\/?[A-Za-z][^<>]*>/.source;

/**
 * Minimal tokenizer: comments, strings (incl. template + escaped), numbers,
 * keywords, function-call names and HTML tags. Output is HTML-escaped and
 * safe for `dangerouslySetInnerHTML`.
 */
export function highlight(code: string, lang: string): string {
  const family: LangFamily = familyFor(lang);
  if (family === "plain") return escapeHtml(code);

  // Capture groups are laid out as: comments…, string, number, fn, word
  // (plus a tag group for markup). The comment group(s) come first.
  const comments = commentSources(family);
  const sources = [...comments];
  if (family === "markup") sources.push(TAG_RE);
  const fnOffset = sources.length; // index of string group
  sources.push(STRING_RE, NUMBER_RE, FN_RE, WORD_RE);

  const re = new RegExp(sources.map((s) => `(${s})`).join("|"), "g");
  const kw = family === "markup" ? null : KEYWORDS[family as Exclude<LangFamily, "markup" | "plain">] ?? null;

  let out = "";
  let last = 0;
  let m: RegExpExecArray | null;
  while ((m = re.exec(code))) {
    const idx = m.index;
    const token = m[0];
    let cls: TokenClass = null;
    const group = m.slice(1).findIndex((g) => g !== undefined);
    if (group >= 0 && group < comments.length) {
      cls = "tok-com";
    } else if (family === "markup" && group === comments.length) {
      cls = "tok-tag";
    } else if (group === fnOffset) {
      cls = "tok-str";
    } else if (group === fnOffset + 1) {
      cls = "tok-num";
    } else if (group === fnOffset + 2) {
      cls = kw && kw.includes(token) ? "tok-kw" : "tok-fn";
    } else if (group === fnOffset + 3) {
      cls = kw && kw.includes(token) ? "tok-kw" : null;
    }

    out += escapeHtml(code.slice(last, idx));
    if (cls) {
      out += `<span class="${cls}">${escapeHtml(token)}</span>`;
    } else {
      out += escapeHtml(token);
    }
    last = idx + token.length;
  }
  out += escapeHtml(code.slice(last));
  return out;
}

function commentSources(family: LangFamily): string[] {
  switch (family) {
    case "python":
      return [/#[^\n]*/.source, /"""[\s\S]*?"""|'''[\s\S]*?'''/.source];
    case "shell":
      return [/#[^\n]*/.source];
    case "markup":
      return [/<!--[\s\S]*?-->/.source, /\/\*[\s\S]*?\*\//.source];
    default:
      return [/\/\/[^\n]*/.source, /\/\*[\s\S]*?\*\//.source];
  }
}

/** Infer a highlighting family from a file path. */
export function languageFor(path: string): string {
  const name = path.toLowerCase();
  const ext = name.split(".").pop() ?? "";
  if (/\.(ts|tsx|mts|cts)$/.test(name)) return "ts";
  if (/\.(js|jsx|mjs|cjs|json|jsonc)$/.test(name)) return "ts";
  if (ext === "rs") return "rust";
  if (ext === "py" || ext === "pyi" || ext === "ipynb") return "python";
  if (ext === "sh" || ext === "bash" || ext === "zsh" || ext === "fish") return "shell";
  if (/\.(html?|css|scss|sass|less|xml|svg|md|markdown|mdx)$/.test(name)) return "markup";
  return "";
}

function familyFor(lang: string): LangFamily {
  const known: LangFamily[] = ["ts", "rust", "python", "markup", "shell"];
  return known.includes(lang as LangFamily) ? (lang as LangFamily) : "plain";
}