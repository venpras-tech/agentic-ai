import { describe, expect, it } from "vitest";

import {
  ACK_SLOW_AFTER_MS,
  STALE_AFTER_MS,
  initialChatStatus,
  isActivePhase,
  reduceChatStatus,
  statusView,
  type ChatStatusEvent,
} from "./chatStatus";

function drive(events: ChatStatusEvent[]) {
  return events.reduce(reduceChatStatus, initialChatStatus);
}

describe("reduceChatStatus lifecycle", () => {
  it("starts in idle and ignores stream events without an active turn", () => {
    const s = drive([
      { type: "token", sessionId: 1, len: 5, at: 10 },
      { type: "done", sessionId: 1, at: 20 },
    ]);
    expect(s).toEqual(initialChatStatus);
  });

  it("walks sending → started → token → done", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 7, at: 100 },
      { type: "token", sessionId: 7, len: 4, at: 200 },
      { type: "token", sessionId: 7, len: 6, at: 300 },
      { type: "done", sessionId: 7, at: 400 },
    ]);
    expect(s.phase).toBe("complete");
    expect(s.chars).toBe(10);
    expect(s.sessionId).toBe(7);
  });

  it("steps move streaming into working with a label", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 1, at: 1 },
      { type: "step", sessionId: 1, step: 2, group: "Execute", at: 50 },
    ]);
    expect(s.phase).toBe("working");
    expect(s.label).toBe("step 2 · Execute");
  });

  it("tool running sets label; tool done clears it only if it owns the label", () => {
    const base: ChatStatusEvent[] = [
      { type: "submit", at: 0 },
      { type: "started", sessionId: 1, at: 1 },
    ];
    const running = drive([
      ...base,
      { type: "tool", tool: "read_file", status: "running", summary: "read a.rs", at: 10 },
    ]);
    expect(running.phase).toBe("working");
    expect(running.label).toBe("read a.rs");

    const cleared = reduceChatStatus(running, {
      type: "tool",
      tool: "read_file",
      status: "done",
      summary: "read a.rs",
      at: 20,
    });
    expect(cleared.phase).toBe("working");
    expect(cleared.label).toBeNull();

    const kept = reduceChatStatus(
      { ...running, label: "subtask 1/2 · plan" },
      { type: "tool", tool: "read_file", status: "done", summary: "read a.rs", at: 30 },
    );
    expect(kept.label).toBe("subtask 1/2 · plan");
  });

  it("permission keeps the turn visibly working", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 3, at: 1 },
      { type: "permission", tool: "execute_terminal_command", at: 10 },
    ]);
    expect(s.phase).toBe("working");
    expect(s.label).toContain("approval");
  });

  it("error is terminal but recoverable by a new submit", () => {
    const errored = drive([
      { type: "submit", at: 0 },
      { type: "error", message: "boom", at: 5 },
    ]);
    expect(errored.phase).toBe("error");
    expect(errored.error).toBe("boom");
    const again = drive([{ type: "submit", at: 0 }, { type: "error", message: "x", at: 5 }, { type: "submit", at: 100 }]);
    expect(again.phase).toBe("sending");
    expect(again.error).toBeUndefined();
  });

  it("ignores tokens from a stale session during an active turn", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 1, at: 1 },
      { type: "token", sessionId: 9, len: 99, at: 2 },
    ]);
    expect(s.phase).not.toBe("streaming");
    expect(s.chars).toBe(0);
  });

  it("does not start a second turn while one is active", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 1, at: 1 },
      { type: "submit", at: 500 },
    ]);
    expect(s.phase).toBe("thinking");
    expect(s.sessionId).toBe(1);
  });

  it("reset returns to pristine idle state", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 1, at: 1 },
      { type: "reset", at: 2 },
    ]);
    expect(s).toEqual(initialChatStatus);
  });
});

describe("statusView", () => {
  it("flags slow ack while sending past the threshold", () => {
    const s = drive([{ type: "submit", at: 0 }]);
    expect(statusView(s, ACK_SLOW_AFTER_MS - 1).stale).toBe(false);
    expect(statusView(s, ACK_SLOW_AFTER_MS + 1).stale).toBe(true);
    expect(statusView(s, ACK_SLOW_AFTER_MS + 1).label).toContain("Console");
  });

  it("flags staleness for quiet working turns past STALE_AFTER_MS", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 1, at: 1 },
      { type: "step", sessionId: 1, step: 1, at: 10 },
    ]);
    expect(statusView(s, 10 + STALE_AFTER_MS - 1).stale).toBe(false);
    expect(statusView(s, 10 + STALE_AFTER_MS + 1).stale).toBe(true);
  });

  it("terminal phases are never stale and carry plain labels", () => {
    const done = reduceChatStatus(
      drive([
        { type: "submit", at: 0 },
        { type: "started", sessionId: 2, at: 1 },
      ]),
      { type: "done", sessionId: 2, at: 999_999 },
    );
    expect(done.phase).toBe("complete");
    const view = statusView(done, 999_999 + STALE_AFTER_MS * 10);
    expect(view.stale).toBe(false);
    expect(view.label).toBe("Done");
  });

  it("announces phase for screen readers", () => {
    const s = drive([
      { type: "submit", at: 0 },
      { type: "started", sessionId: 1, at: 1 },
    ]);
    expect(statusView(s, 5).announcement).toMatch(/in progress/i);
  });
});

describe("phase helpers", () => {
  it("classifies active phases", () => {
    expect(isActivePhase("sending")).toBe(true);
    expect(isActivePhase("thinking")).toBe(true);
    expect(isActivePhase("streaming")).toBe(true);
    expect(isActivePhase("working")).toBe(true);
    expect(isActivePhase("idle")).toBe(false);
    expect(isActivePhase("complete")).toBe(false);
    expect(isActivePhase("error")).toBe(false);
  });
});
