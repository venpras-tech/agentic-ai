import { describe, expect, it } from "vitest";
import {
  applyPatchSelection,
  hunkAdditionLines,
  hunkCurrentRange,
  parseUnifiedDiffHunks,
} from "./unifiedPatch";
const BEFORE = [
  "line1",
  "line2",
  "line3",
  "line4",
  "line5",
].join("\n");

// Replace "line3" -> "LINE3" with the surrounding context included.
const SINGLE = `--- a/f
+++ b/f
@@ -2,3 +2,3 @@
 line2
-line3
+LINE3
 line4
`;

describe("unifiedPatch", () => {
  it("parses a single hunk with old/new ranges", () => {
    const hunks = parseUnifiedDiffHunks(SINGLE);
    expect(hunks).toHaveLength(1);
    expect(hunks[0].oldStart).toBe(2);
    expect(hunks[0].oldCount).toBe(3);
    expect(hunks[0].newStart).toBe(2);
    expect(hunks[0].newCount).toBe(3);
  });

  it("applies an included hunk", () => {
    const hunks = parseUnifiedDiffHunks(SINGLE);
    const out = applyPatchSelection(BEFORE, hunks, () => true);
    expect(out).toBe(["line1", "line2", "LINE3", "line4", "line5"].join("\n"));
  });

  it("skips a rejected hunk (keeps original lines)", () => {
    const hunks = parseUnifiedDiffHunks(SINGLE);
    const out = applyPatchSelection(BEFORE, hunks, () => false);
    expect(out).toBe(BEFORE);
  });

  it("computes the added-line span", () => {
    const hunks = parseUnifiedDiffHunks(SINGLE);
    expect(hunkAdditionLines(hunks, hunks[0])).toEqual([3]);
    expect(hunkCurrentRange(hunks, hunks[0])).toEqual({ start: 2, end: 4 });
  });

  it("rejects one hunk of a two-hunk patch and keeps the other", () => {
    const raw = `--- a/f
+++ b/f
@@ -1,2 +1,2 @@
 line1
-line2
+LINE2
@@ -4,2 +4,2 @@
 line4
-line5
+LINE5
`;
    const hunks = parseUnifiedDiffHunks(raw);
    expect(hunks).toHaveLength(2);
    // Keep only the second hunk (lines 4-5) → line2 stays original.
    const out = applyPatchSelection(BEFORE, hunks, (h) => h.oldStart === 4);
    expect(out).toBe(["line1", "line2", "line3", "line4", "LINE5"].join("\n"));
    // Added-line spans shift after the first hunk is applied.
    expect(hunkAdditionLines(hunks, hunks[1])).toEqual([5]);
  });

  it("preserves a trailing newline exactly", () => {
    const base = "a\nb\nc\n";
    const raw = `--- a/f
+++ b/f
@@ -3,1 +3,1 @@
-c
+C
`;
    const hunks = parseUnifiedDiffHunks(raw);
    expect(applyPatchSelection(base, hunks, () => true)).toBe("a\nb\nC\n");
  });

  it("in-place replacement without terminal newline round-trips", () => {
    const base = "a\nb\nc"; // no trailing newline
    const raw = `--- a/f
+++ b/f
@@ -2,1 +2,1 @@
-b
+B
`;
    const hunks = parseUnifiedDiffHunks(raw);
    expect(applyPatchSelection(base, hunks, () => true)).toBe("a\nB\nc");
  });

  it("a rejected hunk that adds lines leaves later hunks correctly positioned", () => {
    const raw = `--- a/f
+++ b/f
@@ -1,2 +1,3 @@
 line1
 line2
+INSERTED
@@ -2,1 +3,1 @@
 line2
-line3
+LINE3
`;
    const hunks = parseUnifiedDiffHunks(raw);
    expect(hunks).toHaveLength(2);
    // Reject the first (INSERTED) hunk, keep the second → line3 replaced still.
    const out = applyPatchSelection(BEFORE, hunks, (h) => h.oldStart !== 1);
    expect(out).toBe(["line1", "line2", "LINE3", "line4", "line5"].join("\n"));
  });

  it("hunkAdditionLines maps additions across an inserted block", () => {
    const raw = `--- a/f
+++ b/f
@@ -1,2 +1,4 @@
 line1
 line2
+INSERTED
+INSERTED2
`;
    const hunks = parseUnifiedDiffHunks(raw);
    expect(hunkAdditionLines(hunks, hunks[0])).toEqual([3, 4]);
    expect(hunkCurrentRange(hunks, hunks[0])).toEqual({ start: 1, end: 4 });
  });
});
