import { useEffect, useMemo, useRef } from "react";
import * as monaco from "monaco-editor";

import {
  applyPatchSelection,
  hunkAdditionLines,
  parseUnifiedDiffHunks,
} from "../lib/unifiedPatch";

export interface PendingDiffSection {
  /** Stable identity shared with the reducer (e.g. `${messageIndex}-${diffIndex}`). */
  key: string;
  /** Original (pre-change) file content — the diff base. */
  before: string;
  /** Unified diff text (may contain several hunks). */
  diff: string | null;
}

export interface HunkViewProps {
  editor: monaco.editor.IStandaloneCodeEditor;
  /** Path of the open file (for decorations keying). */
  path: string | null;
  /** Pending diffs whose `before`/`diff` describe this file's changes. */
  sections: PendingDiffSection[];
  /**
   * Fired when the user toggles a hunk: `keep=true` writes content with the
   * hunk applied, `keep=false` writes it with the hunk discarded. The caller
   * persists to disk, updates the editor buffer, and reconciles resolution
   * state.
   */
  onToggleHunk: (opts: {
    before: string;
    diff: string | null;
    key: string;
    keep: boolean;
    content: string;
  }) => void;
  /** map of "si:hi" hunk key -> whether it is kept (applied). */
  resolution?: Record<string, boolean> | null;
}

function hunkKey(sectionKey: string, hi: number): string {
  return `${sectionKey}#${hi}`;
}

/**
 * Renders per-hunk Monaco decorations (gutter markers + line highlights) for
 * the authored unified diffs of the currently open file, with hover actions to
 * keep / discard an individual hunk.
 */
export default function HunkDecorations({
  editor,
  path,
  sections,
  onToggleHunk,
  resolution,
}: HunkViewProps) {
  const parsed = useMemo(
    () =>
      sections.map((s) => ({
        key: s.key,
        before: s.before,
        hunks: parseUnifiedDiffHunks(s.diff ?? ""),
      })),
    [sections],
  );

  const onToggleHunkRef = useRef(onToggleHunk);
  onToggleHunkRef.current = onToggleHunk;
  const resolutionRef = useRef(resolution);
  resolutionRef.current = resolution;

  const parsedRef = useRef(parsed);
  parsedRef.current = parsed;

  const hoverActionRef = useRef<{
    si: number;
    hi: number;
  } | null>(null);

  const decorationsRef = useRef<monaco.editor.IEditorDecorationsCollection | null>(null);
  const hoverRef = useRef<monaco.IDisposable | null>(null);
  const disposablesRef = useRef<monaco.IDisposable[]>([]);

  function isKept(sectionKey: string, hi: number): boolean {
    const r = resolutionRef.current;
    if (!r) return true;
    const v = r[hunkKey(sectionKey, hi)];
    return v === undefined ? true : v;
  }

  function toggleHunk(si: number, hi: number, keep: boolean) {
    const sec = parsed[si];
    const key = hunkKey(sec.key, hi);
    const content = applyPatchSelection(sec.before, sec.hunks, (h) => {
      const idx = sec.hunks.indexOf(h);
      if (idx < 0) return true;
      if (idx === hi) return keep;
      return isKept(sec.key, idx);
    });
    onToggleHunkRef.current({
      before: sec.before,
      diff: sections[si]?.diff ?? null,
      key,
      keep,
      content,
    });
  }

  useEffect(() => {
    const controller = editor.createDecorationsCollection();
    decorationsRef.current = controller;

    // Hover contents in Monaco are IMarkdownString[] — we route the buttons
    // through `command:` links dispatched to a registered editor command.
    const CMD_APPLY = "hunk-view.apply";
    const CMD_REVERT = "hunk-view.revert";
    const disp = [
      monaco.editor.addCommand({ id: CMD_APPLY, run: () => {
        const h = hoverActionRef.current;
        if (h) toggleHunk(h.si, h.hi, true);
      } }),
      monaco.editor.addCommand({ id: CMD_REVERT, run: () => {
        const h = hoverActionRef.current;
        if (h) toggleHunk(h.si, h.hi, false);
      } }),
    ];
    disposablesRef.current = disp;

    const hover = monaco.languages.registerHoverProvider("*", {
      provideHover: (model, position) => {
        const editorUri = editor.getModel()?.uri.toString();
        if (!editorUri || model.uri.toString() !== editorUri) return null;
        for (let si = 0; si < parsedRef.current.length; si++) {
          const sec = parsedRef.current[si];
          const hunks = sec.hunks;
          for (let hi = 0; hi < hunks.length; hi++) {
            const lines = hunkAdditionLines(hunks, hunks[hi]);
            if (!lines.includes(position.lineNumber)) continue;
            const kept = isKept(sec.key, hi);
            hoverActionRef.current = { si, hi };
            const action = kept ? CMD_REVERT : CMD_APPLY;
            const actionLabel = kept ? "Revert hunk" : "Apply hunk";
            return {
              range: new monaco.Range(
                position.lineNumber,
                1,
                position.lineNumber,
                model.getLineMaxColumn(position.lineNumber),
              ),
              contents: [
                {
                  value:
                    `**Authored change**  \n` +
                    `Apply this hunk to the file, or discard it back to its pre-change state.  \n` +
                    `[${actionLabel}](command:${action})`,
                },
              ],
            };
          }
        }
        return null;
      },
    });
    hoverRef.current = hover;

    return () => {
      controller.clear();
      hover.dispose();
      disp.forEach((d) => d.dispose());
      disposablesRef.current = [];
      hoverActionRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor]);

  useEffect(() => {
    const controller = decorationsRef.current;
    const model = editor.getModel();
    if (!controller || !model) return;

    const decos: monaco.editor.IModelDeltaDecoration[] = [];
    for (let si = 0; si < parsed.length; si++) {
      const sec = parsed[si];
      const hunks = sec.hunks;
      for (let hi = 0; hi < hunks.length; hi++) {
        const lines = hunkAdditionLines(hunks, hunks[hi]);
        const kept = isKept(sec.key, hi);
        for (const ln of lines) {
          if (ln < 1 || ln > model.getLineCount()) continue;
          decos.push({
            range: new monaco.Range(ln, 1, ln, 1),
            options: {
              isWholeLine: true,
              linesDecorationsClassName: kept ? "hunk-glyph" : "hunk-glyph hunk-glyph-reverted",
              className: kept ? "hunk-added-line" : "hunk-reverted-line",
            },
          });
        }
      }
    }
    controller.set(decos);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editor, parsed, resolution]);

  // `path` is intentionally unused for decoration placement but kept so callers
  // can re-key on file switch. It influences nothing here.
  void path;

  return null;
}
