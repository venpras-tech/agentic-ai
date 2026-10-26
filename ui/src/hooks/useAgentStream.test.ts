import { describe, it, expect, beforeEach } from "vitest";
import {
  appendRunTrace,
  mergeEditBatch,
  claimNextSession,
  clearActiveSession,
  isActiveSession,
  resetActiveSession,
  setActiveSessionId,
} from "./useAgentStream";
import type { EditBatchEvent, EditStatus, FileEdit, PendingEditGroup, RunTrace } from "../lib/ipc";

const edit = (id: string, status: EditStatus = "pending", path = `src/${id}.ts`): FileEdit =>
  ({ id, path, kind: "patch", status }) as FileEdit;

const event = (groupId: string, edits: FileEdit[]): EditBatchEvent => ({
  sessionId: 1,
  groupId,
  edits,
  timestampMs: Date.now(),
});

const group = (id: string, edits: FileEdit[]): PendingEditGroup => ({
  id,
  sessionId: 1,
  tool: "edit",
  edits,
});

const step = (stepIndex: number, overrides: Partial<RunTrace> = {}): RunTrace => ({
  sessionId: 1,
  stepIndex,
  group: "group",
  tools: ["edit_file"],
  durationMs: 1200,
  tokensIn: 100,
  tokensOut: 20,
  outcome: "completed",
  timestampMs: Date.now(),
  ...overrides,
});

describe("appendRunTrace", () => {
  it("appends steps in arrival order", () => {
    const next = appendRunTrace(appendRunTrace([], step(1)), step(2));
    expect(next.map((s) => s.stepIndex)).toEqual([1, 2]);
  });

  it("keeps the list untouched for a malformed event", () => {
    const prev = [step(1)];
    expect(appendRunTrace(prev, undefined as unknown as RunTrace)).toBe(prev);
    expect(appendRunTrace(prev, { stepIndex: "x" } as unknown as RunTrace)).toBe(prev);
  });

  it("replaces a repeat of a step already recorded", () => {
    const prev = [step(1), step(2)];
    const next = appendRunTrace(prev, step(1, { outcome: "failed", durationMs: 30 }));
    expect(next).toHaveLength(2);
    expect(next[1]).toMatchObject({ stepIndex: 2 });
    expect(next[0]).toMatchObject({ outcome: "failed", durationMs: 30 });
  });
});

describe("mergeEditBatch", () => {
  it("seeds a brand-new group from a propose event", () => {
    const next = mergeEditBatch([], event("edit-1", [edit("ed-1"), edit("ed-2")]));
    expect(next).toHaveLength(1);
    expect(next[0]).toMatchObject({ id: "edit-1", edits: [{ id: "ed-1" }, { id: "ed-2" }] });
  });

  it("folds an apply result into the existing group", () => {
    const prev = [group("edit-1", [edit("ed-1"), edit("ed-2")])];
    const next = mergeEditBatch(
      prev,
      event("edit-1", [edit("ed-1", "applied"), edit("ed-2", "pending")]),
    );
    expect(next).toHaveLength(1);
    expect(next[0].edits.map((e) => e.status)).toEqual(["applied", "pending"]);
  });

  it("drops a group once nothing is pending anymore", () => {
    const prev = [group("edit-1", [edit("ed-1")])];
    const next = mergeEditBatch(prev, event("edit-1", [edit("ed-1", "applied")]));
    expect(next).toHaveLength(0);
  });

  it("keeps a group while some edits are still pending", () => {
    const prev = [group("edit-1", [edit("ed-1"), edit("ed-2")])];
    const next = mergeEditBatch(
      prev,
      event("edit-1", [edit("ed-1", "applied"), edit("ed-2", "pending")]),
    );
    expect(next).toHaveLength(1);
    expect(next[0].edits.some((e) => e.status === "pending")).toBe(true);
  });

  it("leaves other groups untouched by an unrelated batch", () => {
    const prev = [group("edit-9", [edit("ed-9")])];
    const next = mergeEditBatch(prev, event("edit-1", [edit("ed-1", "applied")]));
    expect(next).toEqual(prev);
  });

  it("is a no-op for an event without a group id", () => {
    const prev = [group("edit-1", [edit("ed-1")])];
    expect(mergeEditBatch(prev, event("", [edit("ed-1", "applied")]))).toEqual(prev);
  });

  it("keeps a conflict group actionable until resolved (failed is not actionable)", () => {
    const prev = [group("edit-1", [edit("ed-1"), edit("ed-2")])];
    const next = mergeEditBatch(
      prev,
      event("edit-1", [edit("ed-1", "conflict", "src/a.ts"), edit("ed-2", "failed")]),
    );
    expect(next).toHaveLength(1);
    expect(next[0].edits.map((e) => e.status)).toEqual(["conflict", "failed"]);
  });
});

describe("isActiveSession", () => {
  beforeEach(() => {
    resetActiveSession();
  });

  it("accepts events while the view owns nothing (external runs stay visible)", () => {
    expect(isActiveSession(7)).toBe(true);
    expect(isActiveSession(undefined)).toBe(true);
  });

  it("accepts only the claimed run once a session id is pinned", () => {
    setActiveSessionId(42);
    expect(isActiveSession(42)).toBe(true);
    expect(isActiveSession(43)).toBe(false);
  });

  it("adopts the first event after a claim, then rejects other runs", () => {
    claimNextSession();
    // The backend emits `inference-started` before the invoke resolves, so the
    // first event adopts the run this view just started.
    expect(isActiveSession(9)).toBe(true);
    expect(isActiveSession(9)).toBe(true);
    // A leftover run from the previous chat must not bleed into this one.
    expect(isActiveSession(3)).toBe(false);
  });

  it("rejects foreign runs after the transcript is replaced", () => {
    setActiveSessionId(5);
    clearActiveSession();
    // "New chat" releases the pin but keeps ownership: a stale run still
    // flushing its final events must not write into the fresh transcript.
    expect(isActiveSession(5)).toBe(false);
    expect(isActiveSession(8)).toBe(false);
  });

  it("re-adopts after a fresh claim once the view stops owning a run", () => {
    setActiveSessionId(5);
    clearActiveSession();
    claimNextSession();
    expect(isActiveSession(11)).toBe(true);
    expect(isActiveSession(5)).toBe(false);
  });
});