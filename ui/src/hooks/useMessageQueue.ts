import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { useApp } from "../context/AppContext";
import { api } from "../lib/ipc";

/** A message typed while the agent was busy, held until the run ends. */
interface QueuedMessage {
  id: string;
  text: string;
}

export interface MessageQueue {
  queue: QueuedMessage[];
  steeringId: string | null;
  enqueueMessage: (text: string) => void;
  steerQueued: (id: string) => Promise<void>;
  dequeueMessage: (id: string) => void;
}

/**
 * The steer queue: messages submitted while a run is in flight. Each entry can
 * be pushed into the live run (steer), dropped, or left to send automatically
 * once the run ends.
 */
export function useMessageQueue(
  isProcessing: boolean,
  sendPrompt: (text: string) => Promise<void>,
): MessageQueue {
  const { setComposerDraft, addActivity } = useApp();
  const [queue, setQueue] = useState<QueuedMessage[]>([]);
  const [steeringId, setSteeringId] = useState<string | null>(null);
  // Mirrors `queue` so the drain effect and steer handler can read the current
  // list without re-subscribing on every render.
  const queueRef = useRef<QueuedMessage[]>([]);
  queueRef.current = queue;

  /** Park a message and clear the composer so the user can keep typing. */
  const enqueueMessage = useCallback(
    (text: string) => {
      const item = {
        id: `q-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        text,
      };
      setQueue((q) => [...q, item]);
      setComposerDraft("");
      addActivity("Queued — the agent is still working on the previous message.", "status");
    },
    [setComposerDraft, addActivity],
  );

  /** Hold a message for later, or push it into the live run when the user
   *  presses Steer. Backend rejects a steer with no run in progress, which is
   *  the authority on "is anything actually running". */
  const steerQueued = useCallback(
    async (id: string) => {
      const item = queueRef.current.find((q) => q.id === id);
      if (!item || steeringId) return;
      setSteeringId(id);
      try {
        await api.agentSteer(item.text);
        setQueue((q) => q.filter((m) => m.id !== id));
        addActivity("Steering the running agent…", "status");
      } catch (err) {
        // Nothing running (or it just finished): fall back to a normal send so
        // the message is not silently lost.
        addActivity(
          `Could not steer (${err instanceof Error ? err.message : String(err)}) — sending as a new message.`,
          "status",
        );
        setQueue((q) => q.filter((m) => m.id !== id));
        await sendPrompt(item.text);
      }
      setSteeringId(null);
    },
    [steeringId, addActivity, sendPrompt],
  );

  const dequeueMessage = useCallback((id: string) => {
    setQueue((q) => q.filter((m) => m.id !== id));
  }, []);

  /** Drain the queue once the run ends, oldest first. */
  useEffect(() => {
    if (isProcessing) return;
    const pending = queueRef.current;
    if (pending.length === 0) return;
    const [next, ...rest] = pending;
    setQueue(rest);
    void sendPrompt(next.text);
  }, [isProcessing, sendPrompt]);

  // A steer the backend applied lands here; drop the queue row so it is not
  // offered twice.
  useEffect(() => {
    const unlisten = listen<{ id: string }>("agent://steer-applied", (e) => {
      setQueue((q) => q.filter((m) => m.id !== e.payload.id));
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, []);

  return { queue, steeringId, enqueueMessage, steerQueued, dequeueMessage };
}
