import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";

// Test isTransientError logic by re-implementing it for testability.
function isTransientError(err: unknown): boolean {
  const msg = err instanceof Error ? err.message : String(err);
  const transient = [
    "channel is closed",
    "channel busy",
    "failed to send",
    "connection closed",
    "resource unavailable",
  ];
  return transient.some((t) => msg.toLowerCase().includes(t));
}

describe("isTransientError", () => {
  it("detects 'channel is closed'", () => {
    expect(isTransientError(new Error("channel is closed"))).toBe(true);
  });

  it("detects 'channel busy'", () => {
    expect(isTransientError(new Error("channel busy"))).toBe(true);
  });

  it("detects 'failed to send'", () => {
    expect(isTransientError(new Error("failed to send IPC message"))).toBe(true);
  });

  it("detects 'connection closed'", () => {
    expect(isTransientError(new Error("connection closed by peer"))).toBe(true);
  });

  it("detects 'resource unavailable'", () => {
    expect(isTransientError(new Error("resource unavailable"))).toBe(true);
  });

  it("is case-insensitive", () => {
    expect(isTransientError(new Error("Channel Is Closed"))).toBe(true);
    expect(isTransientError(new Error("CHANNEL BUSY"))).toBe(true);
  });

  it("rejects non-transient errors", () => {
    expect(isTransientError(new Error("permission denied"))).toBe(false);
    expect(isTransientError(new Error("file not found"))).toBe(false);
    expect(isTransientError(new Error("model not loaded"))).toBe(false);
  });

  it("handles non-Error values", () => {
    expect(isTransientError("channel busy")).toBe(true);
    expect(isTransientError("some other string")).toBe(false);
    expect(isTransientError(null)).toBe(false);
  });
});

// Test retry behavior with mocked invoke.
const MAX_RETRIES = 2;
const RETRY_DELAYS = [200, 500];

function invokeWithRetry<T>(
  invokeFn: (cmd: string, args: unknown) => Promise<T>,
  cmd: string,
  args: unknown,
  attempt: number,
): Promise<T> {
  return invokeFn(cmd, args).catch((err) => {
    if (attempt < MAX_RETRIES && isTransientError(err)) {
      return new Promise<T>((resolve) =>
        setTimeout(
          () =>
            resolve(invokeWithRetry(invokeFn, cmd, args, attempt + 1)),
          RETRY_DELAYS[attempt],
        ),
      );
    }
    throw err;
  });
}

describe("invokeWithRetry", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns immediately on success", async () => {
    const mockInvoke = vi.fn().mockResolvedValue("ok");
    const result = await invokeWithRetry(mockInvoke, "cmd", {}, 0);
    expect(result).toBe("ok");
    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });

  it("retries on transient error and succeeds", async () => {
    const mockInvoke = vi
      .fn()
      .mockRejectedValueOnce(new Error("channel busy"))
      .mockResolvedValue("ok");

    const promise = invokeWithRetry(mockInvoke, "cmd", {}, 0);

    // Advance past first retry delay (200ms).
    await vi.advanceTimersByTimeAsync(200);

    const result = await promise;
    expect(result).toBe("ok");
    expect(mockInvoke).toHaveBeenCalledTimes(2);
  });

  it("retries up to MAX_RETRIES then throws", async () => {
    const mockInvoke = vi
      .fn()
      .mockRejectedValue(new Error("channel busy"));

    const promise = invokeWithRetry(mockInvoke, "cmd", {}, 0).catch((e) => e);

    // Advance through both retry delays.
    await vi.advanceTimersByTimeAsync(200);
    await vi.advanceTimersByTimeAsync(500);

    const err = await promise;
    expect(err).toBeInstanceOf(Error);
    expect((err as Error).message).toBe("channel busy");
    expect(mockInvoke).toHaveBeenCalledTimes(3); // initial + 2 retries
  });

  it("does not retry non-transient errors", async () => {
    const mockInvoke = vi
      .fn()
      .mockRejectedValue(new Error("permission denied"));

    await expect(
      invokeWithRetry(mockInvoke, "cmd", {}, 0),
    ).rejects.toThrow("permission denied");
    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });
});
