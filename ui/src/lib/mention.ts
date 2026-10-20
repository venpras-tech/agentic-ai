import { api } from "./ipc";

/** Directories never worth listing in the `@` typeahead. */
const SKIP_DIRS = new Set([
  "node_modules",
  ".git",
  "target",
  "dist",
  "build",
  ".next",
  ".cache",
  "__pycache__",
  ".venv",
  "venv",
]);

/** Static `@` commands that toggle context state rather than pin a file. */
export const MENTION_COMMANDS: Array<{ token: string; label: string; hint: string }> = [
  { token: "@codebase", label: "@codebase", hint: "Include a workspace layout section" },
  { token: "@git", label: "@git", hint: "Include git status/diff/log" },
  { token: "@terminal", label: "@terminal", hint: "Reference the terminal output" },
  { token: "@web", label: "@web", hint: "Research on the web" },
];

/** Recursively walk the workspace into a flat list of workspace-relative files.
 *  Bounded by `limit` and directory skips so it stays cheap for large repos. */
export async function listWorkspaceFiles(
  projectPath: string,
  limit = 500,
): Promise<string[]> {
  if (!projectPath) return [];
  const out: string[] = [];
  const walk = async (rel: string | null): Promise<void> => {
    if (out.length >= limit) return;
    let nodes: Array<{ name: string; path: string; isDir: boolean }> = [];
    try {
      nodes = await api.listDirectory(projectPath, rel);
    } catch {
      return;
    }
    for (const n of nodes) {
      if (out.length >= limit) return;
      if (n.isDir) {
        if (SKIP_DIRS.has(n.name)) continue;
        await walk(n.path);
      } else {
        // `path` is absolute or workspace-joined; normalize to relative.
        const normalized = n.path.replaceAll("\\", "/");
        const base = projectPath.replaceAll("\\", "/").replace(/\/?$/, "/");
        out.push(normalized.startsWith(base) ? normalized.slice(base.length) : normalized);
      }
    }
  };
  await walk(null);
  return out;
}

/** Filter workspace files for a mention query (substring, case-insensitive),
 *  ranking basename matches first and capping the result count. */
export function filterMentionFiles(files: string[], query: string, max = 12): string[] {
  const q = query.toLowerCase();
  const scored: Array<{ path: string; score: number }> = [];
  for (const f of files) {
    const lower = f.toLowerCase();
    const base = lower.slice(lower.lastIndexOf("/") + 1);
    let score: number;
    if (!q) score = 3;
    else if (base.startsWith(q)) score = 0;
    else if (base.includes(q)) score = 1;
    else if (lower.includes(q)) score = 2;
    else continue;
    scored.push({ path: f, score });
  }
  scored.sort((a, b) => a.score - b.score || a.path.length - b.path.length || a.path.localeCompare(b.path));
  return scored.slice(0, max).map((s) => s.path);
}

/** Inspect the draft before the caret. Returns the active `@` query (text after
 *  the last `@` that is not preceded by a word char) and its start offset, or
 *  null when the caret is not inside a mention token. */
export function activeMention(
  text: string,
  caret: number,
): { query: string; start: number } | null {
  const before = text.slice(0, caret);
  const at = before.lastIndexOf("@");
  if (at < 0) return null;
  // Must start a token: preceding char is whitespace or line start.
  if (at > 0 && !/\s/.test(before[at - 1])) return null;
  const query = before.slice(at + 1);
  // Allow only path-ish characters; stop the token on other punctuation.
  if (/[^\w./@-]/.test(query)) return null;
  return { query, start: at };
}
