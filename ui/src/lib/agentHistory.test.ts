import { describe, it, expect, vi, beforeEach } from "vitest";
import { internals } from "../test/setup";

const invoke = internals.invoke as unknown as ReturnType<typeof vi.fn>;

import { recordToMessage, hydrateContextFromSession } from "./agentHistory";
import type { SessionRecord } from "./ipc";

const rec = (over: Partial<SessionRecord>): SessionRecord =>
  ({ role: "assistant", content: "x", ...over }) as SessionRecord;

describe("recordToMessage", () => {
  it("maps a user record", () => {
    const m = recordToMessage(rec({ role: "user", content: "hi" }));
    expect(m).toMatchObject({ role: "user", content: "hi" });
  });

  it("maps an assistant record", () => {
    const m = recordToMessage(rec({ role: "assistant", content: "hello" }));
    expect(m).toMatchObject({ role: "assistant", content: "hello" });
  });

  it("maps an error record", () => {
    const m = recordToMessage(rec({ role: "error", content: "boom" }));
    expect(m?.role).toBe("error");
  });

  it("drops empty records so hydration does not show blank bubbles", () => {
    expect(recordToMessage(rec({ content: "" }))).toBeNull();
    expect(recordToMessage(rec({ content: "   \n " }))).toBeNull();
  });

  it("defaults a missing role to assistant", () => {
    const m = recordToMessage(rec({ role: undefined as never, content: "x" }));
    expect(m?.role).toBe("assistant");
  });

  it("preserves the timestamp and done payload", () => {
    const m = recordToMessage(
      rec({ ts: 1234, content: "x", done: { outputTokens: 42 } as never }),
    );
    expect(m?.ts).toBe(1234);
    expect(m?.done).toEqual({ outputTokens: 42 });
  });

  it("carries the turn id through so reloaded messages keep a stable key", () => {
    expect(recordToMessage(rec({ turnId: "t-1", content: "x" }))?.turnId).toBe("t-1");
  });

  it("leaves turnId undefined for records written before it existed", () => {
    expect(recordToMessage(rec({ turnId: "", content: "x" }))?.turnId).toBeUndefined();
  });
});

describe("hydrateContextFromSession", () => {
  beforeEach(() => invoke.mockReset());

  it("sets the system prompt then replays every non-empty turn", async () => {
    invoke.mockResolvedValue(undefined);
    await hydrateContextFromSession(
      [rec({ role: "user", content: "hi" }), rec({ role: "assistant", content: "hello" })],
      "SYSTEM",
    );

    expect(invoke).toHaveBeenCalledTimes(3);
    // `context_set_system_prompt` / `context_push_turn` both take `content`.
    expect(invoke.mock.calls[0][0]).toBe("context_set_system_prompt");
    expect(invoke.mock.calls[0][1]).toEqual({ content: "SYSTEM" });
    expect(invoke.mock.calls[1][0]).toBe("context_push_turn");
    expect(invoke.mock.calls[1][1]).toEqual({ role: "user", content: "hi" });
    expect(invoke.mock.calls[2][0]).toBe("context_push_turn");
    expect(invoke.mock.calls[2][1]).toEqual({ role: "assistant", content: "hello" });
  });

  it("skips empty records", async () => {
    invoke.mockResolvedValue(undefined);
    await hydrateContextFromSession([rec({ content: "" })], "SYSTEM");
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("continues replaying even if one turn fails", async () => {
    invoke
      .mockResolvedValueOnce(undefined)
      .mockRejectedValueOnce(new Error("full"))
      .mockResolvedValueOnce(undefined);
    await hydrateContextFromSession(
      [
        rec({ role: "user", content: "a" }),
        rec({ role: "assistant", content: "b" }),
        rec({ role: "user", content: "c" }),
      ],
      "SYSTEM",
    );
    expect(invoke).toHaveBeenCalledTimes(4);
  });
});
