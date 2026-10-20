import { describe, it, expect } from "vitest";
import { reorderQueue, editQueueItem, type QueuedMessage } from "./useMessageQueue";

const q = (id: string, text = id): QueuedMessage => ({ id, text, turnId: `t-${id}` });

describe("reorderQueue", () => {
  const base = [q("a"), q("b"), q("c")];

  it("moves an item earlier", () => {
    expect(reorderQueue(base, "b", -1).map((m) => m.id)).toEqual(["b", "a", "c"]);
  });

  it("moves an item later", () => {
    expect(reorderQueue(base, "b", 1).map((m) => m.id)).toEqual(["a", "c", "b"]);
  });

  it("is a no-op at the boundaries", () => {
    expect(reorderQueue(base, "a", -1)).toBe(base);
    expect(reorderQueue(base, "c", 1)).toBe(base);
  });

  it("is a no-op for an unknown id", () => {
    expect(reorderQueue(base, "zzz", 1)).toBe(base);
  });

  it("does not mutate the input array", () => {
    const copy = base.slice();
    reorderQueue(base, "a", 1);
    expect(base).toEqual(copy);
  });
});

describe("editQueueItem", () => {
  it("replaces the text of the matching item only", () => {
    const out = editQueueItem([q("a", "one"), q("b", "two")], "b", "changed");
    expect(out.find((m) => m.id === "b")?.text).toBe("changed");
    expect(out.find((m) => m.id === "a")?.text).toBe("one");
  });

  it("preserves id and turnId", () => {
    const out = editQueueItem([q("a", "one")], "a", "new");
    expect(out[0]).toEqual({ id: "a", text: "new", turnId: "t-a" });
  });

  it("is a no-op for an unknown id", () => {
    const base = [q("a")];
    const out = editQueueItem(base, "zzz", "x");
    expect(out).toEqual(base);
    expect(out).not.toBe(base);
  });
});
