import { useCallback, useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { api } from "../lib/ipc";
import { newTurnId } from "../lib/sessionLog";
import type { SendPromptOptions } from "./useComposerSend";

export interface QueuedMessage {
  id: string;
  text: string;
  turnId: string;
}

/** Swap two adjacent items (dir -1 earlier, +1 later); out-of-range is a no-op. */
export function reorderQueue(
  q: QueuedMessage[],
  id: string,
  dir: -1 | 1,
): QueuedMessage[] {
  const i = q.findIndex((m) => m.id === id);
  const j = i + dir;
  if (i < 0 || j < 0 || j >= q.length) return q;
  const next = q.slice();
  [next[i], next[j]] = [next[j], next[i]];
  return next;
}

/** Replace the text of one queued message in place, preserving id/turnId. */
export function editQueueItem(
  q: QueuedMessage[],
  id: string,
  text: string,
): QueuedMessage[] {
  return q.map((m) => (m.id === id ? { ...m, text } : m));
}

export interface UseMessageQueueOptions {
  isProcessing: boolean;
  sendPrompt: (text: string, opts?: SendPromptOptions) => Promise<void> | void;
  /** Clear just-submitted text and the input-history hint after parking. */
  onEnqueue: () => void;
  /** Show the queued user turn in the transcript immediately. */
  onQueueUserTurn: (text: string, turnId: string) => void;
  addActivity: (text: string, kind: "error" | "status" | "tool" | "index") => void;
}

/** Messages typed while the agent is busy: parked in a queue, steered into
 *  the live run on demand, or drained oldest-first once the run ends. */
export function useMessageQueue({
  isProcessing,
  sendPrompt,
  onEnqueue,
  onQueueUserTurn,
  addActivity,
}: UseMessageQueueOptions) {
  const [queue, setQueue] = useState<QueuedMessage[]>([]);
  const [steeringId, setSteeringId] = useState<string | null>(null);
  // Mirrors `queue` so the drain effect and steer handler can read the current
  // list without re-subscribing on every render.
  const queueRef = useRef<QueuedMessage[]>([]);
  queueRef.current = queue;

  /** Park a message and clear the composer so the user can keep typing. */
  const enqueueMessage = useCallback(
    (text: string) => {
      const turnId = newTurnId();
      const item = {
        id: `q-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
        text,
        turnId,
      };
      onQueueUserTurn(text, turnId);
      setQueue((q) => [...q, item]);
      onEnqueue();
      addActivity("Queued — the agent is still working on the previous message.", "status");
    },
    [onEnqueue, onQueueUserTurn, addActivity],
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

  /** Reorder a parked message one slot earlier (-1) or later (+1). */
  const moveQueued = useCallback((id: string, dir: -1 | 1) => {
    setQueue((q) => reorderQueue(q, id, dir));
  }, []);

  /** Rewrite a parked message in place (keeps its id/turn id). */
  const editQueued = useCallback((id: string, text: string) => {
    setQueue((q) => editQueueItem(q, id, text));
  }, []);

  /** Drain the queue once the run ends, oldest first. */
  useEffect(() => {
    if (isProcessing) return;
    const pending = queueRef.current;
    if (pending.length === 0) return;
    const [next, ...rest] = pending;
    setQueue(rest);
    void sendPrompt(next.text, { skipUserBubble: true, userTurnId: next.turnId });
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

  return { queue, setQueue, steeringId, enqueueMessage, steerQueued, dequeueMessage, moveQueued, editQueued };
}