import { describe, it, expect } from "vitest";
import { selectRewindCheckpoints } from "./rewind";
import type { FileCheckpoint } from "./ipc";

function cp(id: string, path: string, timestampMs: number): FileCheckpoint {
  return { id, path, timestampMs, editId: `${id}-edit`, contentHash: `${id}-hash` };
}

describe("selectRewindCheckpoints", () => {
  it("returns one checkpoint per file, the earliest at or after the boundary", () => {
    const all = [
      cp("a1", "src/a.ts", 100),
      cp("a2", "src/a.ts", 250),
      cp("b1", "src/b.ts", 300),
      cp("a0", "src/a.ts", 50),
    ];
    const picked = selectRewindCheckpoints(all, 200);
    expect(picked.map((c) => c.id).sort()).toEqual(["a2", "b1"]);
  });

  it("ignores edits that happened before the boundary", () => {
    const all = [cp("old", "src/a.ts", 10), cp("new", "src/b.ts", 500)];
    const picked = selectRewindCheckpoints(all, 100);
    expect(picked.map((c) => c.id)).toEqual(["new"]);
  });

  it("is empty when nothing changed after the boundary", () => {
    expect(selectRewindCheckpoints([cp("a", "src/a.ts", 5)], 1000)).toEqual([]);
  });
});
