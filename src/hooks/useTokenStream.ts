import { useCallback, useRef, useState } from "react";

/**
 * Per-session streaming text buffers.
 *
 * Incoming token events are appended to a mutable ref (no re-render) and the
 * committed state is flushed at most once per animation frame. This keeps React
 * work proportional to display frequency instead of token frequency.
 */
export function useTokenStream() {
  const [streams, setStreams] = useState<ReadonlyMap<number, string>>(new Map());
  const pending = useRef(new Map<number, string>());
  const committed = useRef(new Map<number, string>());
  const scheduled = useRef(false);

  const flush = useCallback(() => {
    scheduled.current = false;
    if (pending.current.size === 0) return;
    const next = new Map(committed.current);
    for (const [sessionId, text] of pending.current) next.set(sessionId, text);
    committed.current = next;
    pending.current = new Map();
    setStreams(next);
  }, []);

  const append = useCallback(
    (sessionId: number, delta: string) => {
      const current = pending.current.get(sessionId) ?? committed.current.get(sessionId) ?? "";
      pending.current.set(sessionId, current + delta);
      if (!scheduled.current) {
        scheduled.current = true;
        requestAnimationFrame(flush);
      }
    },
    [flush],
  );

  const clearStream = useCallback((sessionId: number) => {
    pending.current.delete(sessionId);
    committed.current.delete(sessionId);
    setStreams((prev) => {
      if (!prev.has(sessionId)) return prev;
      const next = new Map(prev);
      next.delete(sessionId);
      return next;
    });
  }, []);

  const clearAll = useCallback(() => {
    pending.current.clear();
    committed.current.clear();
    setStreams(new Map());
  }, []);

  return { streams, append, clearStream, clearAll };
}
