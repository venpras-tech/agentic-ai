import { useCallback, useEffect, useRef } from "react";

import { api } from "../lib/ipc";

interface InterruptButtonProps {
  /** Show the button (typically while a generation / tool run is in flight). */
  visible: boolean;
  /** Optional override; defaults to calling `abort_agent_execution`. */
  onAbort?: () => void;
}

/** Two Esc presses within this window trigger an abort. */
const DOUBLE_ESC_WINDOW_MS = 600;

/**
 * Global circuit-breaker control.
 *
 * Renders a red square "abort" button when `visible`, and while visible it
 * installs a window-level keydown hook that treats a *double* Escape press
 * (within 600ms) as an abort - a single Esc is intentionally ignored so it
 * never fires accidentally.
 */
export default function InterruptButton({ visible, onAbort }: InterruptButtonProps) {
  const lastEsc = useRef(0);
  const onAbortRef = useRef(onAbort);
  onAbortRef.current = onAbort;

  const abort = useCallback(() => {
    if (onAbortRef.current) {
      onAbortRef.current();
    } else {
      void api.abortAgentExecution();
    }
  }, []);

  useEffect(() => {
    if (!visible) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.repeat) return;
      const now = Date.now();
      if (now - lastEsc.current < DOUBLE_ESC_WINDOW_MS) {
        lastEsc.current = 0;
        abort();
      } else {
        lastEsc.current = now;
      }
    };
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      lastEsc.current = 0;
    };
  }, [visible, abort]);

  if (!visible) return null;

  return (
    <button
      onClick={abort}
      aria-label="Abort execution"
      title="Abort execution (double-press Esc)"
      className="pointer-events-auto flex h-9 w-9 items-center justify-center rounded-md border border-red-400/50 bg-red-500/15 text-red-300 shadow-lg transition-colors hover:bg-red-500/30"
    >
      <span className="text-[13px] leading-none">■</span>
    </button>
  );
}
