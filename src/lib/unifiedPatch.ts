// Pure unified-diff helpers for the agent-first editor: split a multi-hunk
// patch into individual hunks and replay selected hunks against the original
// (pre-change) file content. Kept dependency-free and side-effect-free so the
// line arithmetic is unit-testable independently of Monaco/React.

export interface DiffHunk {
  /** Hunk header, e.g. `@@ -12,5 +12,7 @@`. */
  header: string;
  /** Start line (1-based) in the ORIGINAL (before) file. */
  oldStart: number;
  /** Number of lines the hunk touches in the original file. */
  oldCount: number;
  /** Start line (1-based) in the NEW (after) file. */
  newStart: number;
  /** Number of lines in the new file. */
  newCount: number;
  /** The raw `+`/`-`/` ` lines (with prefix), excluding the header. */
  lines: string[];
  /** Whether this hunk is currently applied onto the working buffer. */
  applied: boolean;
}

const RANGE_RE = /^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@/;

/** Parse a unified diff into hunks. Non-diff/context lines are ignored. */
export function parseUnifiedDiffHunks(raw: string): DiffHunk[] {
  const text = raw.replace(/\r\n/g, "\n");
  const lines = text.split("\n");
  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;

  for (const line of lines) {
    if (line.startsWith("@@")) {
      const m = RANGE_RE.exec(line);
      if (!m) continue;
      current = {
        header: line,
        oldStart: Number(m[1]),
        oldCount: m[2] ? Number(m[2]) : 1,
        newStart: Number(m[3]),
        newCount: m[4] ? Number(m[4]) : 1,
        lines: [],
        applied: true,
      };
      hunks.push(current);
    } else if (current) {
      if (line.startsWith("+") || line.startsWith("-") || line.startsWith(" ")) {
        current.lines.push(line);
      }
    }
  }
  return hunks;
}

/**
 * Reconstruct the file content after applying only the hunks for which
 * `isApplied(hunk)` returns true, replaying the diff against the original
 * `before` content (all hunks are positioned relative to `before`, so they are
 * applied in oldStart order against the same original line array).
 */
export function applyPatchSelection(
  before: string,
  hunks: DiffHunk[],
  isApplied: (hunk: DiffHunk) => boolean,
): string {
  const lines = before.split("\n");
  const ordered = [...hunks].sort((a, b) => a.oldStart - b.oldStart);

  const out: string[] = [];
  let cursor = 0; // index into `lines` (original) of the next unconsumed line

  for (const hunk of ordered) {
    if (!isApplied(hunk)) continue;
    const start = Math.min(hunk.oldStart - 1, lines.length);
    // Copy unchanged lines up to this hunk's old window.
    for (; cursor < start; cursor++) out.push(lines[cursor] ?? "");
    // Replay the hunk lines against the original buffer.
    for (const dl of hunk.lines) {
      if (dl.startsWith("-") || dl.startsWith(" ")) {
        // Consume one original line (del: drop it; context: still consumed).
        cursor += 1;
      }
      if (dl.startsWith("+") || dl.startsWith(" ")) {
        out.push(dl.slice(1));
      }
    }
  }
  // Tail: remaining original lines.
  for (; cursor < lines.length; cursor++) out.push(lines[cursor] ?? "");
  return out.join("\n");
}

/**
 * Compute the new-file line range a hunk occupies once the FULL patch (all
 * hunks applied) is in effect, given the original `before`. Returns 1-based
 * inclusive [start, end] or null when no new lines land.
 */
export function hunkCurrentRange(
  hunks: DiffHunk[],
  target: DiffHunk,
): { start: number; end: number } | null {
  const ordered = [...hunks].sort((a, b) => a.oldStart - b.oldStart);
  let shift = 0;
  for (const h of ordered) {
    if (h === target) break;
    shift += h.newCount - h.oldCount;
  }
  const start = target.newStart + shift;
  if (target.newCount <= 0) return null;
  return { start, end: start + target.newCount - 1 };
}

/** New-file line numbers occupied by addition (`+`) lines of a hunk. */
export function hunkAdditionLines(
  hunks: DiffHunk[],
  target: DiffHunk,
): number[] {
  const range = hunkCurrentRange(hunks, target);
  if (!range) return [];
  const { start } = range;
  const res: number[] = [];
  let newLine = start;
  for (const dl of target.lines) {
    if (dl.startsWith("+")) res.push(newLine);
    if (dl.startsWith("+") || dl.startsWith(" ")) newLine += 1;
  }
  return res;
}
